//! `latest.json`: an asset of every release, written by CI (`flux-release manifest`).
//!
//! ```json
//! {
//!   "format": 1,
//!   "version": "0.1.0",
//!   "channel": "stable",
//!   "date": "2026-10-20",
//!   "notes": "Markdown of the release notes",
//!   "notes_url": "https://github.com/egor4geev/flux/releases/tag/v0.1.0",
//!   "platforms": {
//!     "macos-aarch64": {
//!       "url": "https://github.com/…/Flux-0.1.0-macos-aarch64.zip",
//!       "size": 31457280,
//!       "sha256": "hex",
//!       "signature": "base64 of the Ed25519 signature of the archive's bytes",
//!       "installer": "https://github.com/…/Flux-0.1.0-macos-aarch64.dmg"
//!     },
//!     "macos-x86_64": { … }
//!   }
//! }
//! ```
//!
//! Unknown keys are ignored: a newer CI may add some. `format` grows only when an older Flux
//! couldn't read the file anymore.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::version::{Channel, Version};

/// The format of `latest.json` this Flux reads.
pub const FORMAT: u32 = 1;

/// The name of the manifest's asset in a release.
pub const ASSET: &str = "latest.json";

/// A release as `latest.json` describes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub version: Version,
    pub channel: Channel,
    /// The release's date, `YYYY-MM-DD`.
    pub date: String,
    /// The release notes, Markdown (English).
    #[serde(default)]
    pub notes: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes_url: Option<String>,
    /// By [`platform_key`].
    pub platforms: BTreeMap<String, Platform>,
}

/// The files of a release for one platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Platform {
    /// The update archive (`.zip` of `Flux.app`, made by `ditto -c -k --keepParent`).
    pub url: String,
    pub size: u64,
    /// Lowercase hex.
    pub sha256: String,
    /// Base64 of the Ed25519 signature of the archive's bytes.
    pub signature: String,
    /// The installer for a first install (`.dmg`), shown when Flux can't update itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installer: Option<String>,
}

impl Manifest {
    /// Reads `latest.json`; a newer `format` is an error ("update Flux by hand").
    pub fn parse(bytes: &[u8]) -> Result<Manifest, String> {
        if let Some((format, version)) = head(bytes)
            && format > FORMAT
        {
            let release = version
                .map(|v| format!("Flux {v}"))
                .unwrap_or("The release".into());
            return Err(format!(
                "{release} is described in format {format} of {ASSET}; this Flux reads format \
                 {FORMAT}: download it from the release page"
            ));
        }
        serde_json::from_slice(bytes).map_err(|err| format!("{ASSET} can't be read: {err}"))
    }

    /// This Mac's files, if the release has them.
    pub fn platform(&self) -> Option<&Platform> {
        self.platforms.get(&platform_key())
    }
}

/// The format of a manifest and its version, read even when the rest isn't readable: a newer
/// format is told apart from a broken file.
pub(crate) fn head(bytes: &[u8]) -> Option<(u32, Option<Version>)> {
    #[derive(Deserialize)]
    struct Head {
        format: u32,
        #[serde(default)]
        version: Option<String>,
    }
    let head: Head = serde_json::from_slice(bytes).ok()?;
    let version = head.version.and_then(|text| Version::parse(&text).ok());
    Some((head.format, version))
}

/// This build's platform: `macos-aarch64`, `macos-x86_64` (later `linux-x86_64`…).
pub fn platform_key() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn sample() -> Manifest {
        let mut platforms = BTreeMap::new();
        platforms.insert(
            "macos-aarch64".to_string(),
            Platform {
                url: "https://example.com/Flux-0.2.0-macos-aarch64.zip".into(),
                size: 3,
                sha256: "ab".repeat(32),
                signature: "c2ln".into(),
                installer: Some("https://example.com/Flux-0.2.0-macos-aarch64.dmg".into()),
            },
        );
        Manifest {
            format: FORMAT,
            version: Version::parse("0.2.0-beta.1").unwrap(),
            channel: Channel::Beta,
            date: "2026-10-20".into(),
            notes: "## Fixed\n- things".into(),
            notes_url: Some("https://github.com/egor4geev/flux/releases/tag/v0.2.0-beta.1".into()),
            platforms,
        }
    }

    #[test]
    fn round_trip() {
        let manifest = sample();
        let json = serde_json::to_vec_pretty(&manifest).unwrap();
        assert_eq!(Manifest::parse(&json).unwrap(), manifest);
        let text = String::from_utf8(json).unwrap();
        assert!(text.contains("\"version\": \"0.2.0-beta.1\""), "{text}");
        assert!(text.contains("\"channel\": \"beta\""), "{text}");
    }

    #[test]
    fn unknown_keys_and_missing_optionals() {
        let json = br#"{"format":1,"version":"0.1.0","channel":"stable","date":"2026-10-20",
            "future":{"x":1},"platforms":{"macos-x86_64":{"url":"u","size":1,"sha256":"s",
            "signature":"g","delta":"d"}}}"#;
        let manifest = Manifest::parse(json).unwrap();
        assert_eq!(manifest.notes, "");
        assert_eq!(manifest.notes_url, None);
        assert_eq!(manifest.platforms["macos-x86_64"].installer, None);
    }

    #[test]
    fn newer_format() {
        let json = br#"{"format":2,"version":"1.0.0","whatever":true}"#;
        let err = Manifest::parse(json).unwrap_err();
        assert!(
            err.contains("Flux 1.0.0") && err.contains("format 2"),
            "{err}"
        );
        assert_eq!(
            head(json),
            Some((2, Some(Version::parse("1.0.0").unwrap())))
        );
        assert!(Manifest::parse(b"{nope").is_err());
    }

    #[test]
    fn this_platform() {
        let key = platform_key();
        assert!(key.contains('-'));
        let manifest = sample();
        assert_eq!(manifest.platform().is_some(), key == "macos-aarch64");
    }
}
