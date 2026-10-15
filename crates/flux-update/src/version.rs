//! Versions and channels. Stable releases are tags `vX.Y.Z` on `main`, betas — `vX.Y.Z-beta.N` on
//! `pre-release` (a pre-release on GitHub). The Beta channel gets both: whichever is newer.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

pub use semver::Version;

/// The channel of updates (Settings → Updates).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    Beta,
}

impl Channel {
    /// The channel a build belongs to: a pre-release version (`0.2.0-beta.1`) — Beta.
    pub fn of(version: &Version) -> Channel {
        if version.pre.is_empty() {
            Channel::Stable
        } else {
            Channel::Beta
        }
    }

    /// The settings' spelling (`"stable"`, `"beta"`).
    pub fn key(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
        }
    }

    pub fn from_key(key: &str) -> Option<Channel> {
        match key {
            "stable" => Some(Channel::Stable),
            "beta" => Some(Channel::Beta),
            _ => None,
        }
    }
}

/// This build's version (the workspace's, `CARGO_PKG_VERSION`). `FLUX_BUILD_VERSION` at build time
/// overrides it: the manual check of updates builds an "older" Flux this way.
pub fn current_version() -> Version {
    option_env!("FLUX_BUILD_VERSION")
        .and_then(|text| Version::parse(text.trim()).ok())
        .unwrap_or_else(|| {
            Version::parse(env!("CARGO_PKG_VERSION")).expect("the workspace's version is SemVer")
        })
}

/// Whether `candidate` is an update for `current` (SemVer order: `0.2.0-beta.1` < `0.2.0`; build
/// metadata after `+` doesn't count).
pub fn is_newer(candidate: &Version, current: &Version) -> bool {
    candidate.cmp_precedence(current) == Ordering::Greater
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn order() {
        let chain = [
            "0.1.0",
            "0.2.0-beta.1",
            "0.2.0-beta.2",
            "0.2.0-beta.10",
            "0.2.0",
        ];
        for pair in chain.windows(2) {
            assert!(
                is_newer(&v(pair[1]), &v(pair[0])),
                "{} > {}",
                pair[1],
                pair[0]
            );
            assert!(
                !is_newer(&v(pair[0]), &v(pair[1])),
                "{} < {}",
                pair[0],
                pair[1]
            );
        }
        assert!(!is_newer(&v("0.2.0"), &v("0.2.0")));
        assert!(!is_newer(&v("0.2.0+ci.7"), &v("0.2.0")));
        assert!(is_newer(&v("0.10.0"), &v("0.9.3")));
    }

    #[test]
    fn channels() {
        assert_eq!(Channel::of(&v("0.1.0")), Channel::Stable);
        assert_eq!(Channel::of(&v("0.2.0-beta.1")), Channel::Beta);
        assert_eq!(Channel::of(&v("0.2.0+build")), Channel::Stable);
        for channel in [Channel::Stable, Channel::Beta] {
            assert_eq!(Channel::from_key(channel.key()), Some(channel));
        }
        assert_eq!(Channel::from_key("nightly"), None);
        assert_eq!(serde_json::to_string(&Channel::Beta).unwrap(), "\"beta\"");
    }

    #[test]
    fn current() {
        let version = current_version();
        if option_env!("FLUX_BUILD_VERSION").is_none() {
            assert_eq!(version.to_string(), env!("CARGO_PKG_VERSION"));
        }
    }
}
