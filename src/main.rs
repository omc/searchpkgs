use clap::Parser;
use lazy_static::lazy_static;
use regex::Regex;
use semver::Version;
use serde::{Deserialize, Serialize};
use snafu::prelude::*;
use std::{
    collections::BTreeMap,
    fmt::Display,
    fs::File,
    io::{BufReader, BufWriter},
    path::{Path, PathBuf},
    sync::Arc,
};
use strum::{EnumIter, IntoEnumIterator};
use tokio::{
    sync::mpsc::{UnboundedReceiver, UnboundedSender},
    task::JoinSet,
};
use tracing::{info, instrument};
use tracing_subscriber;
use url::Url;

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
    let (manifest_tx, manifest_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut set = JoinSet::new();
    for (engine, versions) in engine_versions {
        let manifest = manifest.clone();
        set.spawn(generate_hashes_for_engine_and_arch(
            engine,
            versions,
            manifest,
            manifest_tx.clone(),
        ));
    }

    let manifest_task = tokio::task::spawn(async { update_manifest(manifest_rx).await });

    // let the work run
    while let Some(Ok(_)) = set.join_next().await {}
    // let _ = tx.send(());

    // one last flush
    let manifest = manifest_task.await.unwrap();
    flush_manifest(&manifest);

    Ok(())
}

fn initialize_manifest() -> Result<Manifest> {
    let path = Path::new("./manifest.json");
    if path.exists() {
        let file = File::open(path).context(FileOpenSnafu)?;
        let reader = BufReader::new(file);
        Ok(serde_json::from_reader(reader).context(ReadManifestSnafu)?)
    } else {
        Ok(Manifest::new())
    }
}

#[instrument]
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

#[instrument(skip(manifest_rx))]
async fn update_manifest(mut manifest_rx: UnboundedReceiver<ManifestTuple>) -> Manifest {
    let mut manifest = Manifest::new();
    while let Some((engine, version, arch, url, hash)) = manifest_rx.recv().await {
        info!("Updating manifest for {engine} {version} {arch}");
        let details = Details { hash, url };
        manifest
            .entry(engine)
            .or_default()
            .entry(version)
            .or_default()
            .entry(arch)
            .or_insert(details);
        flush_manifest(&manifest);
    }
    manifest
}

#[instrument(skip(manifest))]
fn flush_manifest(manifest: &Manifest) {
    let file = File::create(Path::new("./manifest.json")).unwrap();
    let writer = BufWriter::new(file);
    serde_json::to_writer_pretty(writer, &manifest).unwrap();
}

#[instrument(skip(versions, manifest, manifest_tx))]
async fn generate_hashes_for_engine_and_arch(
    engine: Engine,
    versions: Vec<Version>,
    manifest: Manifest,
    manifest_tx: UnboundedSender<ManifestTuple>,
) {
    let client = reqwest::Client::new();
    let mut set = JoinSet::new();
    let concurrency = 2;
    for version in versions {
        for arch in Arch::iter() {
            // TODO: model systems: linux, darwin
            // check for an entry and avoid redoing work
            let details = manifest
                .get(&engine)
                .and_then(|ev| ev.get(&version))
                .and_then(|foo| foo.get(&arch));
            if details.is_some() {
                info!("Skipping {engine} {version} {arch}");
                continue;
            }
            // limit concurrency per engine
            while set.len() >= concurrency {
                set.join_next().await.unwrap().unwrap();
            }
            // begin another task when able
            set.spawn(generate_hash(
                engine,
                version.clone(),
                arch,
                manifest_tx.clone(),
                client.clone(),
            ));
        }
    }
    while let Some(Ok(_)) = set.join_next().await {}
}

#[instrument]
async fn generate_hash(
    engine: Engine,
    version: Version,
    arch: Arch,
    manifest_tx: UnboundedSender<ManifestTuple>,
    client: reqwest::Client,
) {
    // if the hash exists in the manifest, return quickly
    let url = get_url(&engine, &version, &arch).unwrap();
    let hash: NixHash = get_hash(url.clone(), client).await;
    manifest_tx
        .send((engine, version, arch, url, hash))
        .unwrap();
}

#[instrument]
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
    let file = File::create_new(path.into()).context(FileOpenSnafu)?;
    let writer = BufWriter::new(file);
    serde_json::to_writer_pretty(writer, &engine_versions).context(VersionJsonWriteSnafu)?;
    Ok(engine_versions)
}

#[instrument]
fn versions_from_file(path: impl Into<PathBuf> + std::fmt::Debug) -> Result<EngineVersions> {
    let file = File::open(path.into()).context(FileOpenSnafu)?;
    let reader = BufReader::new(file);
    serde_json::from_reader(reader).context(VersionJsonParseSnafu)
}

#[instrument]
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
type Manifest = BTreeMap<Engine, BTreeMap<Version, BTreeMap<Arch, Details>>>;
type ManifestTuple = (Engine, Version, Arch, Url, NixHash);
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
    fn opensearch(&self) -> String {
        match self {
            Arch::X86_64 => "x64".into(),
            Arch::Aarch64 => "arm64".into(),
        }
    }

    fn elasticsearch_quickwit(&self) -> String {
        match self {
            Arch::X86_64 => "x86_64".into(),
            Arch::Aarch64 => "aarch64".into(),
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct Details {
    hash: NixHash,
    url: Url,
}

fn get_url(engine: &Engine, version: &Version, arch: &Arch) -> Result<Url> {
    match engine {
        Engine::Elasticsearch => get_elasticsearch_url(version, arch),
        Engine::OpenSearch => get_opensearch_url(version, arch),
        Engine::Quickwit => get_quickwit_url(version, arch),
    }
}

fn get_opensearch_url(version: &Version, arch: &Arch) -> Result<Url> {
    let arch = arch.opensearch();
    format!("https://artifacts.opensearch.org/releases/core/opensearch/{version}/opensearch-min-{version}-linux-{arch}.tar.gz").parse().context(ParseUrlSnafu)
}

fn get_elasticsearch_url(version: &Version, arch: &Arch) -> Result<Url> {
    if *version < Version::parse("7.0.0").unwrap() {
        format!("https://download.elastic.co/elasticsearch/elasticsearch/elasticsearch-{version}.tar.gz").parse().context(ParseUrlSnafu)
    } else {
        let arch = arch.elasticsearch_quickwit();
        format!("https://artifacts.elastic.co/downloads/elasticsearch/elasticsearch-{version}-linux-{arch}.tar.gz").parse().context(ParseUrlSnafu)
    }
}

fn get_quickwit_url(version: &Version, arch: &Arch) -> Result<Url> {
    let arch = arch.elasticsearch_quickwit();
    format!("https://github.com/quickwit-oss/quickwit/releases/download/v{version}/quickwit-v{version}-{arch}-unknown-linux-gnu.tar.gz").parse().context(ParseUrlSnafu)
}

#[instrument]
async fn get_hash(url: Url, client: reqwest::Client) -> NixHash {
    let resp = client.get(url.clone()).send().await.unwrap();
    // TODO: check the status to avoid hashing and caching an error response
    let text = resp.text().await.unwrap();
    let bytes = text.as_bytes();
    nix_sha256_base32(bytes).unwrap()
}

#[instrument(skip(reader))]
fn nix_sha256_base32<R: std::io::Read>(reader: R) -> Result<NixHash> {
    let digest = sha256_digest(reader)?;
    Ok(nix_base32::to_nix_base32(digest.as_ref()))
}

#[instrument(skip(reader))]
fn sha256_digest<R: std::io::Read>(mut reader: R) -> Result<ring::digest::Digest> {
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = [0; 1024];

    loop {
        let count = reader.read(&mut buffer).context(DigestReadSnafu)?;
        if count == 0 {
            break;
        }
        context.update(&buffer[..count]);
    }

    Ok(context.finish())
}

#[instrument]
async fn fetch_quickwit_versions(octocrab: Arc<octocrab::Octocrab>) -> Result<Vec<Version>> {
    fetch_versions_from_release_names(&octocrab, "quickwit-oss", "quickwit").await
}

#[instrument]
async fn fetch_opensearch_versions(octocrab: Arc<octocrab::Octocrab>) -> Result<Vec<Version>> {
    fetch_versions_from_release_names(&octocrab, "opensearch-project", "Opensearch").await
}

#[instrument]
async fn fetch_elasticsearch_versions(octocrab: Arc<octocrab::Octocrab>) -> Result<Vec<Version>> {
    fetch_versions_from_tags(&octocrab, "elastic", "elasticsearch").await
}

#[instrument(skip(octocrab))]
async fn fetch_versions_from_tags(
    octocrab: &Arc<octocrab::Octocrab>,
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
#[instrument(skip(octocrab))]
async fn fetch_versions_from_release_names(
    octocrab: &Arc<octocrab::Octocrab>,
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

fn extract_version_string<'a>(str: &str) -> Option<String> {
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

#[tokio::test]
async fn test_github_client_basics() {}

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Snafu)]
enum Error {
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
