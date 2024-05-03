use lazy_static::lazy_static;
use regex::Regex;
use semver::Version;
use serde::Serialize;
use snafu::prelude::*;
use std::{
    collections::BTreeMap,
    fmt::Display,
    sync::{Arc, Mutex},
};
use strum::{EnumIter, IntoEnumIterator};
use tokio::{
    sync::{mpsc, oneshot::Receiver},
    task::JoinSet,
};
use url::Url;

#[tokio::main]
async fn main() -> Result<()> {
    let octocrab = octocrab::instance();

    let (opensearch_versions, elasticsearch_versions, quickwit_versions) = tokio::join!(
        fetch_opensearch_versions(octocrab.clone()),
        fetch_elasticsearch_versions(octocrab.clone()),
        fetch_quickwit_versions(octocrab.clone())
    );

    let mut versions = BTreeMap::new();
    versions.insert(Engine::Elasticsearch, elasticsearch_versions?);
    versions.insert(Engine::OpenSearch, opensearch_versions?);
    versions.insert(Engine::Quickwit, quickwit_versions?);

    let manifest = build_manifest(versions).await;

    println!("{}", serde_json::to_string_pretty(&manifest).unwrap());
    Ok(())
}

type Manifest = BTreeMap<Engine, BTreeMap<Version, BTreeMap<Arch, Details>>>;

#[derive(Debug, Clone, Serialize, EnumIter, Ord, Eq, PartialOrd, PartialEq, Copy)]
enum Engine {
    Elasticsearch,
    OpenSearch,
    Quickwit,
}

#[derive(Serialize, Ord, PartialOrd, Eq, PartialEq, EnumIter, Clone, Debug, Copy)]
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

#[derive(Serialize)]
struct Details {
    #[serde(skip)]
    engine: Engine,
    #[serde(skip)]
    version: Version,
    #[serde(skip)]
    arch: Arch,

    hash: Option<String>,
    url: Url,

    #[serde(skip)]
    hash_rx: Option<Receiver<String>>,
}

impl Details {
    fn new(engine: Engine, version: Version, arch: Arch) -> Self {
        let url = Self::get_url(&engine, &version, &arch)
            .expect("url generation should generally be fine");
        let hash = None;
        let hash_rx = None;
        Self {
            engine,
            version,
            arch,
            hash,
            url,
            hash_rx,
        }
    }

    async fn get_hash(&mut self, client: reqwest::Client) {
        let url = &self.url;
        let resp = client.get(url.clone()).send().await.unwrap();
        let text = resp.text().await.unwrap();
        let bytes = text.as_bytes();
        // TODO: take advangage of Read and Write to stream these bytes into the digest
        self.hash = Some(nix_sha256_base32(bytes).unwrap());
    }

    fn get_url(engine: &Engine, version: &Version, arch: &Arch) -> Result<Url> {
        match engine {
            Engine::Elasticsearch => Details::get_elasticsearch_url(version, arch),
            Engine::OpenSearch => Details::get_opensearch_url(version, arch),
            Engine::Quickwit => Details::get_quickwit_url(version, arch),
        }
    }

    fn get_opensearch_url(version: &Version, arch: &Arch) -> Result<Url> {
        let arch = arch.opensearch();
        format!("https://artifacts.opensearch.org/releases/core/opensearch/{version}/opensearch-min-{version}-linux-{arch}.tar.gz").parse().context(ParseUrlSnafu)
    }

    fn get_elasticsearch_url(version: &Version, arch: &Arch) -> Result<Url> {
        let arch = arch.elasticsearch_quickwit();
        format!("https://artifacts.elastic.co/downloads/elasticsearch/elasticsearch-{version}-linux-{arch}.tar.gz").parse().context(ParseUrlSnafu)
    }

    fn get_quickwit_url(version: &Version, arch: &Arch) -> Result<Url> {
        let arch = arch.elasticsearch_quickwit();
        format!("https://github.com/quickwit-oss/quickwit/releases/download/v{version}/quickwit-v{version}-{arch}-unknown-linux-gnu.tar.gz").parse().context(ParseUrlSnafu)
    }
}

async fn build_manifest(engine_versions: BTreeMap<Engine, Vec<Version>>) -> Manifest {
    let manifest = Arc::new(Mutex::new(Manifest::new()));
    let mut join_set: JoinSet<Result<()>> = JoinSet::new();
    let max_concurrent = 10;
    let client = reqwest::Client::new();

    for engine in Engine::iter() {
        manifest
            .lock()
            .unwrap()
            .insert(engine.clone(), BTreeMap::new());
        let Some(versions) = engine_versions.get(&engine) else {
            continue;
        };
        for version in versions {
            let version = version.clone();
            manifest
                .lock()
                .unwrap()
                .get_mut(&engine)
                .unwrap()
                .insert(version.clone(), BTreeMap::new());
            for arch in Arch::iter() {
                let manifest = manifest.clone();
                let client = client.clone();
                let version = version.clone();

                // calculating the hash has network IO to fetch the data, and a
                // bit of compute for the hash itself, so we do this async

                // first we wait for room on the join set as a cheap way to limit concurrency
                while join_set.len() >= max_concurrent {
                    let _ = join_set.join_next().await.unwrap().unwrap();
                }

                // now that we're satisfied about concurrency, add another task.
                // we give it a nicely packaged manifest to mutate with its results.
                join_set.spawn(async move {
                    let mut d = Details::new(engine, version.clone(), arch);
                    d.get_hash(client).await;
                    manifest
                        .lock()
                        .unwrap()
                        .get_mut(&engine)
                        .unwrap()
                        .get_mut(&version)
                        .unwrap()
                        .insert(arch, d);
                    Ok(())
                });
            }
        }
    }

    // now we just wait for the rest of the tasks to finish
    while let Some(Ok(_foo)) = join_set.join_next().await {}

    // all done being async, unwrap and return the manifest
    Arc::into_inner(manifest)
        .expect("we're done with the arc")
        .into_inner()
        .expect("we're done with mutex")
}

async fn fetch_quickwit_versions(octocrab: Arc<octocrab::Octocrab>) -> Result<Vec<Version>> {
    fetch_versions_from_release_names(&octocrab, "quickwit-oss", "quickwit").await
}

async fn fetch_opensearch_versions(octocrab: Arc<octocrab::Octocrab>) -> Result<Vec<Version>> {
    fetch_versions_from_release_names(&octocrab, "opensearch-project", "Opensearch").await
}

async fn fetch_elasticsearch_versions(octocrab: Arc<octocrab::Octocrab>) -> Result<Vec<Version>> {
    fetch_versions_from_tags(&octocrab, "elastic", "elasticsearch").await
}

async fn fetch_versions_from_tags(
    octocrab: &Arc<octocrab::Octocrab>,
    owner: impl Into<String>,
    repo: impl Into<String>,
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
        .map(|name| Ok(Version::parse(&name).context(VersionParseFromTagSnafu { name })?))
        .collect();
    let mut v = v?;
    v.sort();
    Ok(v)
}

// TODO: normalize into the structure we expect?
async fn fetch_versions_from_release_names(
    octocrab: &Arc<octocrab::Octocrab>,
    owner: impl Into<String>,
    repo: impl Into<String>,
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
        .map(|name| Ok(Version::parse(&name).context(VersionParseFromReleaseSnafu { name })?))
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

fn nix_sha256_base32<R: std::io::Read>(mut reader: R) -> Result<String> {
    let digest = sha256_digest(reader)?;
    Ok(nix_base32::to_nix_base32(digest.as_ref()))
}
