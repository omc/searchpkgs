use semver::Version;
use snafu::prelude::*;
use std::{
    collections::{btree_map, BTreeMap},
    fmt::Display,
    sync::Arc,
};

struct Manifest {
    opensearch: BTreeMap<Version, ManifestVersion>,
    elasticsearch: BTreeMap<Version, ManifestVersion>,
    quickwit: BTreeMap<Version, ManifestVersion>,
}

struct ManifestVersion {
    x86_64: ManifestVersionDetails,
    aarch64: ManifestVersionDetails,
}

struct ManifestVersionDetails {
    arch: Arch,
    hash: String,
    url: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let octocrab = octocrab::instance();

    let (opensearch_versions, elasticsearch_versions, quickwit_versions) = tokio::join!(
        fetch_opensearch_versions(octocrab.clone()),
        fetch_elasticsearch_versions(octocrab.clone()),
        fetch_quickwit_versions(octocrab.clone())
    );

    if let Ok(versions) = quickwit_versions {
        print_versions("Quickwit", versions);
    }
    if let Ok(versions) = opensearch_versions {
        print_versions("OpenSearch", versions);
    }
    if let Ok(versions) = elasticsearch_versions {
        print_versions("Elasticsearch", versions);
    }

    let _m = manifest();

    Ok(())
}

fn manifest() -> Manifest {
    let opensearch = BTreeMap::new();
    let elasticsearch = BTreeMap::new();
    let quickwit = BTreeMap::new();
    Manifest {
        opensearch,
        elasticsearch,
        quickwit,
    }
}

fn print_versions(name: &str, versions: Vec<Version>) {
    println!("{name} versions: {}", versions.len());
    for version in versions {
        println!("{version}");
    }
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

use lazy_static::lazy_static;
use regex::Regex;

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

struct Artifact {
    variant: Variant,
    version: Version,
}

enum Variant {
    opensearch,
    opensearch_min,
}

impl Display for Variant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Variant::opensearch => f.write_str("opensearch"),
            Variant::opensearch_min => f.write_str("opensearch-min"),
        }
    }
}

// TODO: turn this into a trait
impl Artifact {
    fn new(variant: Variant, version: Version) -> Self {
        Self { variant, version }
    }
    fn linux_artifact_url(&self, arch: Arch) -> String {
        let variant = &self.variant;
        let version = &self.version;
        format!("https://artifacts.opensearch.org/releases/core/opensearch/{version}/{variant}-{version}-linux-{arch}.tar.gz")
    }
    fn linux_artifact_sig_url(&self, arch: Arch) -> String {
        let artifact_url = self.linux_artifact_url(arch);
        format!("{artifact_url}.sig")
    }
    // TODO: https://opensearch.org/verify-signatures.html
}

enum Arch {
    x64,
    arm64,
}

impl Display for Arch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Arch::x64 => f.write_str("x64"),
            Arch::arm64 => f.write_str("arm64"),
        }
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
}
