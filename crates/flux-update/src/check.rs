//! The newest release of a channel.
//!
//! - Stable: `https://github.com/egor4geev/flux/releases/latest/download/latest.json` — GitHub's
//!   "latest" is the newest release that isn't a pre-release; a redirect to the asset, no API
//!   (whose limit is 60 requests an hour per address).
//! - Beta: the API's list of releases (`/repos/…/releases?per_page=20`), the newest version among
//!   them, published (not drafts), with a `latest.json` asset; that asset is read.
//!
//! `FLUX_UPDATE_FEED` — a `latest.json` to read instead, for both channels (`file://…`,
//! `http://127.0.0.1:…/latest.json`): manual checks and scenarios.

use serde::Deserialize;

use crate::REPOSITORY;
use crate::http::{self, Failure};
use crate::manifest::{self, ASSET, FORMAT, Manifest};
use crate::version::{Channel, Version, current_version, is_newer};

/// The largest `latest.json` (or list of releases) Flux reads.
const MAX_JSON: u64 = 8 * 1024 * 1024;

/// The newest release of a channel and whether it's newer than this build.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub manifest: Manifest,
    /// Newer than [`crate::current_version`] and has files for this platform.
    pub is_update: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckError {
    /// No network, the server didn't answer, a timeout.
    Offline(String),
    /// The server answered with something unreadable or an error status.
    Server(String),
    /// The release is for a newer format of `latest.json`: update Flux from the release page.
    UnsupportedFormat { version: Option<Version> },
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckError::Offline(message) => write!(f, "No connection to GitHub: {message}"),
            CheckError::Server(message) => write!(f, "{message}"),
            CheckError::UnsupportedFormat {
                version: Some(version),
            } => write!(
                f,
                "Flux {version} can't be installed by this version: download it from the release \
                 page"
            ),
            CheckError::UnsupportedFormat { version: None } => write!(
                f,
                "The new version can't be installed by this one: download it from the release page"
            ),
        }
    }
}

impl std::error::Error for CheckError {}

impl From<Failure> for CheckError {
    fn from(failure: Failure) -> CheckError {
        if failure.offline {
            CheckError::Offline(failure.message)
        } else {
            CheckError::Server(failure.message)
        }
    }
}

/// The newest release of `channel`. Blocks.
pub fn latest(channel: Channel) -> Result<Release, CheckError> {
    let url = match feed() {
        Some(url) => url,
        None => match channel {
            Channel::Stable => {
                format!("https://github.com/{REPOSITORY}/releases/latest/download/{ASSET}")
            }
            Channel::Beta => newest_asset_url()?,
        },
    };
    let bytes = http::read(&url, MAX_JSON, None)?;
    let manifest = parse(&bytes)?;
    Ok(release_of(manifest, &current_version()))
}

/// `FLUX_UPDATE_FEED`: a `latest.json` read instead of GitHub's (a bare path counts as `file://`).
fn feed() -> Option<String> {
    let feed = std::env::var("FLUX_UPDATE_FEED").ok()?;
    let feed = feed.trim();
    if feed.is_empty() {
        return None;
    }
    if feed.starts_with('/') {
        return Some(format!("file://{feed}"));
    }
    Some(feed.to_string())
}

/// Reads `latest.json`: a newer format is [`CheckError::UnsupportedFormat`].
fn parse(bytes: &[u8]) -> Result<Manifest, CheckError> {
    if let Some((format, version)) = manifest::head(bytes)
        && format > FORMAT
    {
        return Err(CheckError::UnsupportedFormat { version });
    }
    Manifest::parse(bytes).map_err(CheckError::Server)
}

fn release_of(manifest: Manifest, current: &Version) -> Release {
    let is_update = is_newer(&manifest.version, current) && manifest.platform().is_some();
    Release {
        manifest,
        is_update,
    }
}

/// The Beta channel: the `latest.json` of the newest published release, a beta or not.
fn newest_asset_url() -> Result<String, CheckError> {
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases?per_page=20");
    let bytes = http::read(&url, MAX_JSON, Some("application/vnd.github+json"))?;
    match newest_asset(&bytes).map_err(CheckError::Server)? {
        Some((_, url)) => Ok(url),
        None => Err(CheckError::Server(
            "No release of Flux with updates has been published yet".into(),
        )),
    }
}

/// From the API's list of releases: the newest version among published ones (not drafts) that
/// have a `latest.json`, with that asset's URL.
fn newest_asset(releases: &[u8]) -> Result<Option<(Version, String)>, String> {
    #[derive(Deserialize)]
    struct GitHubRelease {
        tag_name: String,
        #[serde(default)]
        draft: bool,
        #[serde(default)]
        assets: Vec<Asset>,
    }
    #[derive(Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
    }
    let releases: Vec<GitHubRelease> = serde_json::from_slice(releases)
        .map_err(|err| format!("GitHub's list of releases can't be read: {err}"))?;
    Ok(releases
        .into_iter()
        .filter(|release| !release.draft)
        .filter_map(|release| {
            let tag = release
                .tag_name
                .strip_prefix('v')
                .unwrap_or(&release.tag_name);
            let version = Version::parse(tag).ok()?;
            let asset = release
                .assets
                .into_iter()
                .find(|asset| asset.name == ASSET)?;
            Some((version, asset.browser_download_url))
        })
        .max_by(|a, b| a.0.cmp_precedence(&b.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASES: &str = r#"[
      {"tag_name": "v0.3.0-beta.1", "draft": true, "prerelease": true,
       "assets": [{"name": "latest.json", "browser_download_url": "https://x/draft.json"}]},
      {"tag_name": "v0.2.0-beta.2", "draft": false, "prerelease": true,
       "assets": [{"name": "Flux.zip", "browser_download_url": "https://x/zip"},
                  {"name": "latest.json", "browser_download_url": "https://x/b2.json"}]},
      {"tag_name": "v0.2.0-beta.3", "draft": false, "prerelease": true,
       "assets": [{"name": "Flux.zip", "browser_download_url": "https://x/no-manifest"}]},
      {"tag_name": "v0.1.0", "draft": false, "prerelease": false,
       "assets": [{"name": "latest.json", "browser_download_url": "https://x/s.json"}]},
      {"tag_name": "nightly", "assets": [{"name": "latest.json", "browser_download_url": "n"}]}
    ]"#;

    #[test]
    fn beta_takes_the_newest_published_with_a_manifest() {
        let (version, url) = newest_asset(RELEASES.as_bytes()).unwrap().unwrap();
        assert_eq!(version.to_string(), "0.2.0-beta.2");
        assert_eq!(url, "https://x/b2.json");
    }

    #[test]
    fn beta_takes_a_newer_stable() {
        let json = r#"[
          {"tag_name": "v0.2.0", "assets": [{"name": "latest.json", "browser_download_url": "s"}]},
          {"tag_name": "v0.2.0-beta.9", "prerelease": true,
           "assets": [{"name": "latest.json", "browser_download_url": "b"}]}
        ]"#;
        let (version, url) = newest_asset(json.as_bytes()).unwrap().unwrap();
        assert_eq!((version.to_string().as_str(), url.as_str()), ("0.2.0", "s"));
        assert_eq!(newest_asset(b"[]").unwrap(), None);
        assert!(newest_asset(b"{\"message\":\"Not Found\"}").is_err());
    }

    #[test]
    fn update_or_not() {
        let mut manifest = crate::manifest::tests::sample();
        manifest.platforms.insert(
            crate::platform_key(),
            manifest.platforms["macos-aarch64"].clone(),
        );
        let older = Version::parse("0.1.0").unwrap();
        let same = manifest.version.clone();
        assert!(release_of(manifest.clone(), &older).is_update);
        assert!(!release_of(manifest.clone(), &same).is_update);
        manifest.platforms.clear();
        assert!(!release_of(manifest, &older).is_update);
    }

    #[test]
    fn newer_format_is_unsupported() {
        let err = parse(br#"{"format": 7, "version": "2.0.0"}"#).unwrap_err();
        assert_eq!(
            err,
            CheckError::UnsupportedFormat {
                version: Some(Version::parse("2.0.0").unwrap())
            }
        );
        assert!(err.to_string().contains("Flux 2.0.0"));
        assert!(matches!(parse(b"oops"), Err(CheckError::Server(_))));
    }

    #[test]
    fn feed_file() {
        let dir = crate::download::tests::temp_dir("feed");
        let path = dir.join("latest.json");
        let manifest = crate::manifest::tests::sample();
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let bytes = http::read(&format!("file://{}", path.display()), MAX_JSON, None).unwrap();
        assert_eq!(parse(&bytes).unwrap(), manifest);
    }

    /// GitHub itself: `cargo test -p flux-update -- --ignored live --nocapture`.
    #[test]
    #[ignore = "network"]
    fn live() {
        for channel in [Channel::Stable, Channel::Beta] {
            match latest(channel) {
                Ok(release) => println!(
                    "{channel:?}: {} {}",
                    release.manifest.version, release.is_update
                ),
                Err(err) => {
                    println!("{channel:?}: {err}");
                    assert!(!matches!(err, CheckError::Offline(_)), "{err}");
                }
            }
        }
    }
}
