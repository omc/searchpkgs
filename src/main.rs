use clap::Parser;
use futures::{FutureExt, TryFutureExt};
use lazy_static::lazy_static;
use regex::Regex;
use semver::Version;
use serde::{Deserialize, Serialize};
use snafu::prelude::*;
use std::{
    collections::BTreeMap, fmt::Display, fs::File, io::{self, BufReader, BufWriter}, path::{Path, PathBuf}, sync::{Arc, Mutex}
};
use strum::{EnumIter, IntoEnumIterator};
use tokio::{
    sync::mpsc::{UnboundedReceiver, UnboundedSender}, task::JoinSet
};
use tracing::{debug, info, instrument};
use url::Url;

#[derive(Ord, PartialOrd, Eq, PartialEq, Clone, Debug, Copy)]
struct System {
    arch: Arch,
    os: OperatingSystem,
}

impl Serialize for System {
    fn serialize<S>(&self, serializer: S) -> std::prelude::v1::Result<S::Ok, S::Error>
    where
        S: serde::Serializer {
        let arch = self.arch.to_string().to_lowercase();
        let os = self.os.to_string().to_lowercase();
        serializer.serialize_str(&format!("{arch}-{os}"))
    }
} 

#[derive(PartialEq, PartialOrd, Eq, Debug)]
struct PackageName {
    engine: Engine,
    version: Version,
}
impl Ord for PackageName {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.engine.cmp(&other.engine).then(self.version.cmp(&other.version))
    }
}

impl Serialize for PackageName {
    fn serialize<S>(&self, serializer: S) -> std::prelude::v1::Result<S::Ok, S::Error>
    where
        S: serde::Serializer {
            let engine = self.engine.to_string().to_lowercase();
            let version = self.version.to_string().replace('.',"_");
            serializer.serialize_str(&format!("{engine}_{version}"))
    }
}

#[derive(Serialize, Debug)]
struct PackageAttrs {
    #[serde(rename="pname")]
    engine: Engine,
    version: Version,
    url: Url,
    sha256: NixHash,
}
type Packages = BTreeMap<System, BTreeMap<PackageName, PackageAttrs>>;

#[derive(Parser, Debug)]
struct Args {
    /// Refresh the version lists from GitHub
    #[clap(long, default_value = "false")]
    update_versions: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt::init();

    let manifest: Manifest = initialize_manifest()?;

    // gather versions
    let engine_versions = load_engine_versions(&args).await?;

    // calculate hashes
    let (manifest_tx, manifest_rx) = tokio::sync::mpsc::unbounded_channel::<Option<ManifestTuple>>();
    let mut set = JoinSet::new();
    for (engine, versions) in engine_versions {
        let manifest = manifest.clone();
        set.spawn(generate_hashes_for_engine(
            engine,
            versions,
            manifest,
            manifest_tx.clone(),
        ));
    }

    // periodic updates of the manifest
    let manifest_task = tokio::task::spawn(async { update_manifest(manifest, manifest_rx ).await });

    // interruptibility
    let set = Arc::new(Mutex::new(set));
    {
        let manifest_tx = manifest_tx.clone();
        let set = set.clone();
        ctrlc::set_handler(move || {
            info!("shutting down...");
            manifest_tx.send(None).expect("signal manifest flush to stop");
            set.lock().unwrap().abort_all();
        }).expect("error setting up ctrl-c handler");
    }

    // let the work run
    while let Some(Ok(_)) = set.lock().unwrap().join_next().await {}

    // wrap it up!
    info!("Finished!");
    if ! manifest_tx.is_closed() {
        manifest_tx.send(None).expect("signal manifest flush to stop");
        manifest_tx.closed().await;
    }
    let manifest = manifest_task.await.unwrap();
    flush_manifest(&manifest);

    let mut packages = Packages::new();
    for (engine, engine_vals) in manifest {
        for (version, arch_vals) in engine_vals {
            for (arch, os_vals) in arch_vals {
                for (os, details) in os_vals {
                    let system = System { arch, os };
                    let package_name = PackageName { engine, version: version.clone() };
                    let package_attrs = PackageAttrs { engine, version: version.clone(), url: details.url, sha256: details.sha256 };
                    packages.entry(system).or_default().insert(package_name, package_attrs);
                }
            }
        }
    };

    let file = File::create("./packages.json").expect("couldn't create packages.json");
    let writer = BufWriter::new(file);
    serde_json::to_writer_pretty(writer, &packages).unwrap();

    Ok(())
}

fn initialize_manifest() -> Result<Manifest> {
    let path = Path::new("./manifest.json");
    match File::open(path) {
        Ok(file) => {
            let reader = BufReader::new(file);
            Ok(serde_json::from_reader(reader).context(ReadManifestSnafu)?)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Manifest::new()),
        Err(e) => Err(e).context(FileOpenSnafu),
    }
}

#[instrument(skip_all)]
async fn load_engine_versions(args: &Args) -> Result<EngineVersions> {
    let path = Path::new("./versions.json");
    if args.update_versions {
        versions_from_github(path).await
    } else if !path.exists() {
        versions_from_github(path).await
    } else {
        versions_from_file(path)
    }
}

#[instrument(skip_all)]
async fn update_manifest(
    mut manifest: Manifest,
    mut manifest_rx: UnboundedReceiver<Option<ManifestTuple>>,
) -> Manifest {
    // let mut manifest = Manifest::new();
    while let Some(Some((engine, version, arch, system, url, sha256))) = manifest_rx.recv().await {
        info!("Updating manifest for {engine} {version} {arch} {system}");
        let details = Details { sha256, url };
        manifest
            .entry(engine)
            .or_default()
            .entry(version)
            .or_default()
            .entry(arch)
            .or_default()
            .entry(system)
            .or_insert(details);
        flush_manifest(&manifest);
    }
    manifest
}

#[instrument(skip_all)]
fn flush_manifest(manifest: &Manifest) {
    let file = File::create("./manifest.json").expect("crash if we can't create this file");
    let writer = BufWriter::new(file);
    serde_json::to_writer_pretty(writer, &manifest).unwrap();
}

#[instrument(skip_all)]
async fn generate_hashes_for_engine(
    engine: Engine,
    versions: Vec<Version>,
    manifest: Manifest,
    manifest_tx: UnboundedSender<Option<ManifestTuple>>,
) {
    let client = reqwest::Client::new();
    let mut set: JoinSet<Result<(), Arc<Error>>> = JoinSet::new();
    let concurrency = 4;
    let mut url_hash_memo = BTreeMap::new();

    // go breadth first through versions because we have some likelihood of reused artifacts and we want to avoid blocking
    let tuples = itertools::iproduct!(Arch::iter(), OperatingSystem::iter(), versions);
    for (arch, system, version) in tuples {
        if manifest
            .get(&engine)
            .and_then(|foo| foo.get(&version))
            .and_then(|foo| foo.get(&arch))
            .and_then(|foo| foo.get(&system))
            .is_some()
        {
            info!("Skipping {engine} {version} {arch} {system}...");
            continue;
        }
        if manifest_tx.is_closed() {
            return
        }
        let url = get_url(&engine, &version, &arch, &system).expect("url parsing shenanigans");

        let hash = url_hash_memo
            .entry(url.clone())
            .or_insert_with(|| {
                get_artifact_hash(url.clone(), client.clone())
                    .map_err(Arc::new)
                    .shared()
            })
            .clone();

        while set.len() >= concurrency {
            if let Err(e) = set.join_next().await.unwrap().unwrap() {
                debug!("Error calculating hash: {e}");
            };
        }
        // begin another task when able
        let manifest_tx = manifest_tx.clone();
        set.spawn(async move {
            let hash = hash.await.unwrap();
            manifest_tx
                .send(Some((engine, version, arch, system, url, hash)))
                .context(ManifestSendSnafu)
                .map_err(Arc::new)
        });
    }

    while let Some(Ok(_)) = set.join_next().await {}
}

#[instrument(skip_all)]
async fn versions_from_github(
    path: impl Into<PathBuf> + std::fmt::Debug,
) -> Result<EngineVersions> {
    let mut set = JoinSet::new();
    for engine in Engine::iter() {
        set.spawn(fetch_versions(engine));
    }
    let mut engine_versions = EngineVersions::new();
    while let Some(Ok(Ok((engine, versions)))) = set.join_next().await {
        engine_versions.insert(engine, versions);
    }
    let file = File::create(path.into()).context(FileOpenSnafu)?;
    let writer = BufWriter::new(file);
    serde_json::to_writer_pretty(writer, &engine_versions).context(VersionJsonWriteSnafu)?;
    Ok(engine_versions)
}

#[instrument(skip_all)]
fn versions_from_file(path: impl Into<PathBuf> + std::fmt::Debug) -> Result<EngineVersions> {
    let file = File::open(path.into()).context(FileOpenSnafu)?;
    let reader = BufReader::new(file);
    serde_json::from_reader(reader).context(VersionJsonParseSnafu)
}

#[instrument(skip_all)]
async fn fetch_versions(engine: Engine) -> Result<(Engine, Vec<Version>)> {
    let octocrab = octocrab::instance();
    let versions = match engine {
        Engine::Elasticsearch => fetch_elasticsearch_versions(octocrab).await,
        Engine::OpenSearch => fetch_opensearch_versions(octocrab).await,
        Engine::Quickwit => fetch_quickwit_versions(octocrab).await,
    };

    Ok((engine, versions?))
}

type EngineVersions = BTreeMap<Engine, Vec<Version>>;
type Manifest = BTreeMap<Engine, BTreeMap<Version, BTreeMap<Arch, BTreeMap<OperatingSystem, Details>>>>;
type ManifestTuple = (Engine, Version, Arch, OperatingSystem, Url, NixHash);
type NixHash = String;

#[derive(Clone, Copy, Debug, Deserialize, EnumIter, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
enum Engine {
    Elasticsearch,
    OpenSearch,
    Quickwit,
}

#[derive(Deserialize, Serialize, Ord, PartialOrd, Eq, PartialEq, EnumIter, Clone, Debug, Copy)]
#[serde(rename_all = "lowercase")]
enum Arch {
    X86_64,
    Aarch64,
}

impl Arch {
    fn format(&self, engine: Engine) -> String {
        match (self, engine) {
            (Arch::X86_64, Engine::Elasticsearch) => "x86_64",
            (Arch::X86_64, Engine::OpenSearch) => "x64",
            (Arch::X86_64, Engine::Quickwit) => "x86_64",
            (Arch::Aarch64, Engine::Elasticsearch) => "aarch64",
            (Arch::Aarch64, Engine::OpenSearch) => "arm64",
            (Arch::Aarch64, Engine::Quickwit) => "aarch64",
        }
        .into()
    }
}


#[derive(Deserialize, Serialize, Ord, PartialOrd, Eq, PartialEq, EnumIter, Clone, Debug, Copy)]
#[serde(rename_all = "lowercase")]
enum OperatingSystem {
    Linux,
    Darwin,
}
impl OperatingSystem {
    fn format(&self, engine: Engine) -> String {
        match (self, engine) {
            (OperatingSystem::Linux, Engine::Elasticsearch) => "linux",
            (OperatingSystem::Linux, Engine::OpenSearch) => "linux",
            (OperatingSystem::Linux, Engine::Quickwit) => "unknown-linux-gnu",
            (OperatingSystem::Darwin, Engine::Elasticsearch) => "darwin",
            (OperatingSystem::Darwin, Engine::OpenSearch) => "darwin",
            (OperatingSystem::Darwin, Engine::Quickwit) => "apple-darwin",
        }.into()
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct Details {
    sha256: NixHash,
    url: Url,
}

fn get_url(engine: &Engine, version: &Version, arch: &Arch, system: &OperatingSystem) -> Result<Url> {
    match engine {
        Engine::Elasticsearch => get_elasticsearch_url(version, arch, system),
        Engine::OpenSearch => get_opensearch_url(version, arch, system),
        Engine::Quickwit => get_quickwit_url(version, arch, system),
    }
}

fn get_opensearch_url(version: &Version, arch: &Arch, _system: &OperatingSystem) -> Result<Url> {
    let arch = arch.format(Engine::OpenSearch);
    format!("https://artifacts.opensearch.org/releases/core/opensearch/{version}/opensearch-min-{version}-linux-{arch}.tar.gz").parse().context(ParseUrlSnafu)
}

fn get_elasticsearch_url(version: &Version, arch: &Arch, system: &OperatingSystem) -> Result<Url> {
    let arch = arch.format(Engine::Elasticsearch);
    let system = system.to_string().to_lowercase();
    let url = match version.major {
        0 | 1 => format!("https://download.elastic.co/elasticsearch/elasticsearch/elasticsearch-{version}.tar.gz"),
        2..=4 => format!("https://download.elastic.co/elasticsearch/release/org/elasticsearch/distribution/tar/elasticsearch/{version}/elasticsearch-{version}.tar.gz"),
        5 | 6 => format!("https://artifacts.elastic.co/downloads/elasticsearch/elasticsearch-{version}.tar.gz"),
        7     => format!("https://artifacts.elastic.co/downloads/elasticsearch/elasticsearch-{version}-{system}-x86_64.tar.gz"),
        8..   => format!("https://artifacts.elastic.co/downloads/elasticsearch/elasticsearch-{version}-{system}-{arch}.tar.gz"),
        
    };
    url.parse().context(ParseUrlSnafu)
}

fn get_quickwit_url(version: &Version, arch: &Arch, system: &OperatingSystem) -> Result<Url> {
    let arch = arch.format(Engine::Quickwit);
    let system = system.format(Engine::Quickwit);
    format!("https://github.com/quickwit-oss/quickwit/releases/download/v{version}/quickwit-v{version}-{arch}-{system}.tar.gz").parse().context(ParseUrlSnafu)
}

#[instrument(skip_all)]
async fn get_artifact_hash(url: Url, client: reqwest::Client) -> Result<NixHash> {
    info!("Expensive hashing of {url}...");
    let mut resp = client
        .get(url.clone())
        .send()
        .await
        .context(GetArtifactSnafu)?
        .error_for_status()
        .context(GetArtifactStatusSnafu)?;
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    while let Ok(Some(chunk)) = resp.chunk().await {
        debug!("Hashing chunk of len {}", chunk.len());
        context.update(&chunk);
    }
    let digest = context.finish();
    Ok(nix_base32::to_nix_base32(digest.as_ref()))
    // nix-prefetch-url --unpack https://download.elastic.co/elasticsearch/elasticsearch/elasticsearch-0.90.13.tar.gz 
    // let foo = Command::new("nix-prefetch-url").args(["--unpack", &url.to_string()]).stdout(Stdio::null()).output().await;
    // todo!()
}

#[instrument(skip_all)]
async fn fetch_quickwit_versions(octocrab: Arc<octocrab::Octocrab>) -> Result<Vec<Version>> {
    fetch_versions_from_release_names(&octocrab, "quickwit-oss", "quickwit").await
}

#[instrument(skip_all)]
async fn fetch_opensearch_versions(octocrab: Arc<octocrab::Octocrab>) -> Result<Vec<Version>> {
    fetch_versions_from_release_names(&octocrab, "opensearch-project", "Opensearch").await
}

#[instrument(skip_all)]
async fn fetch_elasticsearch_versions(octocrab: Arc<octocrab::Octocrab>) -> Result<Vec<Version>> {
    fetch_versions_from_tags(&octocrab, "elastic", "elasticsearch").await
}

#[instrument(skip_all)]
async fn fetch_versions_from_tags(
    octocrab: &octocrab::Octocrab,
    owner: impl Into<String> + std::fmt::Debug,
    repo: impl Into<String> + std::fmt::Debug,
) -> Result<Vec<Version>> {
    let mut page = octocrab
        .repos(owner.into().clone(), repo.into().clone())
        .list_tags()
        .send()
        .await
        .context(ListTagSnafu)?;
    let mut tags = page.take_items();
    while let Ok(Some(mut next_page)) = octocrab.get_page(&page.next).await {
        tags.extend(next_page.take_items());
        page = next_page;
    }
    let v: Result<Vec<Version>> = tags
        .into_iter()
        .map(|t| t.name)
        .flat_map(|name| extract_version_string(&name))
        .map(|name| Version::parse(&name).context(VersionParseFromTagSnafu { name }))
        .collect();
    let mut v = v?;
    v.sort();
    Ok(v)
}

// TODO: normalize into the structure we expect?
#[instrument(skip_all)]
async fn fetch_versions_from_release_names(
    octocrab: &octocrab::Octocrab,
    owner: impl Into<String> + std::fmt::Debug,
    repo: impl Into<String> + std::fmt::Debug,
) -> Result<Vec<Version>> {
    let mut page = octocrab
        .repos(owner.into(), repo.into())
        .releases()
        .list()
        .per_page(100) // todo: paginate
        .send()
        .await
        .context(ListReleaseSnafu)?;

    let mut releases = page.take_items();
    while let Ok(Some(mut next_page)) = octocrab.get_page(&page.next).await {
        releases.extend(next_page.take_items());
        page = next_page;
    }
    let v: Result<Vec<Version>> = releases
        .into_iter()
        .flat_map(|r| r.name)
        .flat_map(|name| extract_version_string(&name))
        .map(|name| Version::parse(&name).context(VersionParseFromReleaseSnafu { name }))
        .collect();
    let mut v = v?;
    v.sort();
    Ok(v)
}

fn extract_version_string(str: &str) -> Option<String> {
    lazy_static! {
        static ref RE: Regex =
            Regex::new(r"^.*(?<version>[0-9]+\.[0-9]+\.[0-9]+(-[a-z0-9]+)?).*$").unwrap();
    }
    RE.captures(&str.replace(".Beta", "-beta").replace(".RC", "-rc"))
        .and_then(|cap| {
            cap.name("version")
                .map(|version| version.as_str().to_string())
        })
}

#[test]
fn test_extract_version_string() {
    assert_eq!(Some("1.2.3".to_string()), extract_version_string("v1.2.3"));
    assert_eq!(None, extract_version_string("some-beta-prerelease-1"))
}

impl Display for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl Display for Arch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl Display for OperatingSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

#[tokio::test]
async fn test_github_client_basics() {}

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Snafu)]
enum Error {
    ManifestSend { source: tokio::sync::mpsc::error::SendError<Option<ManifestTuple>> },
    GetArtifactStatus {
        source: reqwest::Error,
    },
    GetArtifact {
        source: reqwest::Error,
    },
    ReadManifest {
        source: serde_json::Error,
    },
    VersionJsonParse {
        source: serde_json::Error,
    },
    VersionJsonWrite {
        source: serde_json::Error,
    },
    ListRelease {
        source: octocrab::Error,
    },
    ListTag {
        source: octocrab::Error,
    },
    #[snafu(display("Unable to parse {}", name))]
    VersionParseFromTag {
        source: semver::Error,
        name: String,
    },
    #[snafu(display("Unable to parse {}", name))]
    VersionParseFromRelease {
        source: semver::Error,
        name: String,
    },
    DigestRead {
        source: std::io::Error,
    },
    FileOpen {
        source: std::io::Error,
    },
    ParseUrl {
        source: url::ParseError,
    },
}
