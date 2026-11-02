//! Downloading an update: the archive for this platform into the cache
//! (`~/Library/Caches/Flux/updates/<version>/`), its size, SHA-256 and Ed25519 signature checked
//! against the manifest and [`crate::PUBLIC_KEY`]. An archive already downloaded and valid isn't
//! downloaded again. Flux downloads it itself, so macOS puts no quarantine on it (no Gatekeeper
//! question after an update).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::manifest::{Manifest, platform_key};

/// The error of a download stopped by its cancel flag.
pub const CANCELLED: &str = "Cancelled";

/// The largest archive Flux downloads.
const MAX_ARCHIVE: u64 = 1024 * 1024 * 1024;

/// The cache of updates.
pub fn cache_dir() -> PathBuf {
    let home = flux_platform::paths::home().unwrap_or_default();
    if cfg!(target_os = "macos") {
        return home.join("Library/Caches/Flux/updates");
    }
    if cfg!(windows) {
        return flux_platform::paths::caches_dir_in(&home).join("updates");
    }
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| home.join(".cache"))
        .join("flux/updates")
}

/// Downloads and checks the archive of `manifest` for this platform; returns its path.
/// `progress` gets (bytes so far, all bytes); `cancel` stops the download ([`CANCELLED`]). Blocks.
pub fn download(
    manifest: &Manifest,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<PathBuf, String> {
    let dir = cache_dir().join(manifest.version.to_string());
    download_to(manifest, &dir, crate::PUBLIC_KEY, cancel, progress)
}

/// [`download`] into `dir`, checked by `public_key` (base64).
pub fn download_to(
    manifest: &Manifest,
    dir: &Path,
    public_key: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<PathBuf, String> {
    let platform = manifest.platform().ok_or_else(|| {
        format!(
            "Flux {} has no download for this Mac ({})",
            manifest.version,
            platform_key()
        )
    })?;
    std::fs::create_dir_all(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    let target = dir.join(file_name(&platform.url));
    let check = |bytes: &[u8]| {
        if platform.size != 0 && bytes.len() as u64 != platform.size {
            return Err(format!(
                "The archive's size is {} bytes, the release says {}: it wasn't installed",
                bytes.len(),
                platform.size
            ));
        }
        verify(bytes, &platform.sha256, &platform.signature, public_key)
    };
    if let Ok(bytes) = std::fs::read(&target)
        && check(&bytes).is_ok()
    {
        progress(bytes.len() as u64, bytes.len() as u64);
        return Ok(target);
    }
    let bytes = crate::http::download(&platform.url, platform.size, MAX_ARCHIVE, cancel, progress)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(CANCELLED.into());
    }
    check(&bytes)?;
    let temp = dir.join(format!(
        "{}.flux-tmp",
        target.file_name().unwrap().to_string_lossy()
    ));
    std::fs::write(&temp, &bytes).map_err(|err| format!("{}: {err}", temp.display()))?;
    std::fs::rename(&temp, &target).map_err(|err| format!("{}: {err}", target.display()))?;
    Ok(target)
}

/// Checks an archive's bytes: SHA-256 (hex) and the Ed25519 signature (base64) by `public_key`
/// (base64 of 32 bytes).
pub fn verify(bytes: &[u8], sha256: &str, signature: &str, public_key: &str) -> Result<(), String> {
    let expected = sha256.trim().to_ascii_lowercase();
    let actual = sha256_hex(bytes);
    if actual != expected {
        return Err(format!(
            "The archive's checksum doesn't match the release's (SHA-256 {actual}, expected \
             {expected}): it wasn't installed"
        ));
    }
    crate::signing::verify_signature(bytes, signature, public_key)
}

/// Lowercase hex of the SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The archive's file name: the URL's last segment without a query; no path separators.
fn file_name(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let name = path.rsplit('/').next().unwrap_or_default();
    let name = crate::http::file_path(name)
        .to_string_lossy()
        .replace(['/', '\\', ':'], "_");
    if name.is_empty() || name == "." || name == ".." {
        "Flux.zip".into()
    } else {
        name
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::manifest::{FORMAT, Platform};
    use crate::signing;
    use crate::version::{Channel, Version};

    pub(crate) fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "flux-update-test-{}-{name}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const SEED: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

    fn manifest_for(archive: &Path, bytes: &[u8]) -> Manifest {
        let mut platforms = std::collections::BTreeMap::new();
        platforms.insert(
            platform_key(),
            Platform {
                // `/` between the names on Windows too: the URL's last one is the file's.
                url: format!(
                    "file://{}",
                    archive
                        .display()
                        .to_string()
                        .replace('\\', "/")
                        .replace(' ', "%20")
                ),
                size: bytes.len() as u64,
                sha256: sha256_hex(bytes),
                signature: signing::sign(bytes, SEED).unwrap(),
                installer: None,
            },
        );
        Manifest {
            format: FORMAT,
            version: Version::parse("9.9.9").unwrap(),
            channel: Channel::Stable,
            date: "2026-10-20".into(),
            notes: String::new(),
            notes_url: None,
            platforms,
        }
    }

    #[test]
    fn verify_checks() {
        let public = signing::public_key(SEED).unwrap();
        let bytes = b"Flux.app";
        let sha = sha256_hex(bytes);
        let signature = signing::sign(bytes, SEED).unwrap();
        verify(bytes, &sha, &signature, &public).unwrap();
        verify(bytes, &sha.to_uppercase(), &signature, &public).unwrap();
        let err = verify(bytes, &"0".repeat(64), &signature, &public).unwrap_err();
        assert!(err.contains("checksum"), "{err}");
        let other = signing::sign(b"other", SEED).unwrap();
        let err = verify(bytes, &sha, &other, &public).unwrap_err();
        assert!(err.contains("signature"), "{err}");
        let (_, stranger) = signing::generate().unwrap();
        assert!(verify(bytes, &sha, &signature, &stranger).is_err());
        let err = verify(bytes, &sha, &signature, "bad key").unwrap_err();
        assert!(err.contains("public key"), "{err}");
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn downloads_checks_and_reuses() {
        let source = temp_dir("source");
        let archive = source.join("Flux 9.9.9 macos.zip");
        let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&archive, &bytes).unwrap();
        let manifest = manifest_for(&archive, &bytes);
        let public = signing::public_key(SEED).unwrap();
        let cache = temp_dir("cache");

        let mut seen = Vec::new();
        let path = download_to(
            &manifest,
            &cache,
            &public,
            &AtomicBool::new(false),
            &mut |a, b| seen.push((a, b)),
        )
        .unwrap();
        assert_eq!(path, cache.join("Flux 9.9.9 macos.zip"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(seen.first(), Some(&(0, bytes.len() as u64)));
        assert_eq!(seen.last(), Some(&(bytes.len() as u64, bytes.len() as u64)));
        assert!(seen.len() > 3, "read in pieces: {seen:?}");

        // Already there and valid: not read again (the source is gone).
        std::fs::remove_file(&archive).unwrap();
        let mut calls = 0;
        let again = download_to(
            &manifest,
            &cache,
            &public,
            &AtomicBool::new(false),
            &mut |_, _| calls += 1,
        )
        .unwrap();
        assert_eq!(again, path);
        assert_eq!(calls, 1);

        // A cached file that doesn't check out is downloaded again (and the source is gone).
        std::fs::write(&path, b"tampered").unwrap();
        assert!(
            download_to(
                &manifest,
                &cache,
                &public,
                &AtomicBool::new(false),
                &mut |_, _| {}
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_and_cancels() {
        let source = temp_dir("bad");
        let archive = source.join("Flux.zip");
        std::fs::write(&archive, b"real bytes").unwrap();
        let public = signing::public_key(SEED).unwrap();
        let mut manifest = manifest_for(&archive, b"real bytes");

        let cancelled = download_to(
            &manifest,
            &temp_dir("cancel"),
            &public,
            &AtomicBool::new(true),
            &mut |_, _| {},
        )
        .unwrap_err();
        assert_eq!(cancelled, CANCELLED);

        let (_, stranger) = signing::generate().unwrap();
        let cache = temp_dir("stranger");
        let err = download_to(
            &manifest,
            &cache,
            &stranger,
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .unwrap_err();
        assert!(err.contains("signature"), "{err}");
        assert!(!cache.join("Flux.zip").exists());

        manifest.platforms.get_mut(&platform_key()).unwrap().size = 3;
        let err = download_to(
            &manifest,
            &temp_dir("size"),
            &public,
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .unwrap_err();
        assert!(err.contains("size"), "{err}");

        manifest.platforms.clear();
        let err = download_to(
            &manifest,
            &temp_dir("none"),
            &public,
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .unwrap_err();
        assert!(err.contains("no download for this Mac"), "{err}");
    }

    #[test]
    fn names() {
        assert_eq!(
            file_name("https://github.com/a/b/releases/download/v1/Flux-1-macos-aarch64.zip?x=1"),
            "Flux-1-macos-aarch64.zip"
        );
        assert_eq!(file_name("https://x/"), "Flux.zip");
        assert_eq!(file_name("file:///tmp/My%20Flux.zip"), "My Flux.zip");
    }
}
