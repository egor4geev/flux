//! Updates over the air (part 10.8): Flux reads the newest release of its channel from GitHub
//! Releases, downloads the archive for its platform, checks its SHA-256 and Ed25519 signature,
//! unpacks it next to the cache and, after Flux quits, a small helper swaps `Flux.app` and starts
//! the new version ([[ADR-040 Updates over the Air]] in the wiki).
//!
//! - [`manifest`] — `latest.json`, the release's description (CI writes it with `flux-release`);
//! - [`version`] — versions and channels (Stable: `vX.Y.Z` on `main`, Beta: `vX.Y.Z-beta.N` on
//!   `pre-release`);
//! - [`check`] — the newest release of a channel;
//! - [`download`] — the archive with progress and cancel, its checksum and signature;
//! - [`signing`] — Ed25519 keys and signatures (CI's `flux-release` and the checks here);
//! - [`install`] — unpacking, the new bundle's code signature, the swap after quitting.
//!
//! Blocking calls (network, files, `ditto`, `codesign`): call them off the UI thread.

pub mod check;
pub mod download;
mod http;
pub mod install;
pub mod manifest;
pub mod signing;
pub mod version;

pub use check::{CheckError, Release, latest};
pub use manifest::{Manifest, Platform, platform_key};
pub use version::{Channel, Version, current_version, is_newer};

/// The repository of Flux's releases.
pub const REPOSITORY: &str = "egor4geev/flux";

/// The public half of the key CI signs update archives with (Ed25519, 32 bytes, base64). A build
/// for a manual check of updates may bring its own: `FLUX_UPDATE_PUBLIC_KEY` at build time.
pub const PUBLIC_KEY: &str = match option_env!("FLUX_UPDATE_PUBLIC_KEY") {
    Some(key) => key,
    None => "8a+nn8MrB6v+8Lf2oM2Ne2RwdiUUE73Vw9dVO+8lmGI=",
};

/// The page of a release: the notes, the downloads (`notes_url` of an older manifest may be
/// missing).
pub fn release_page(version: &Version) -> String {
    format!("https://github.com/{REPOSITORY}/releases/tag/v{version}")
}
