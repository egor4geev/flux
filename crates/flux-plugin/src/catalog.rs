//! The plugin catalog (stage 8.3), as the Marketplace of JetBrains IDEs: an index of published
//! plugins, read from the catalog repository (`flux-plugins`: the plugins' sources, their packages
//! in GitHub Releases, plugins of other authors come as pull requests, like Zed's extensions).
//!
//! The index (`index.json`) lists every plugin with its manifest, its package (a `.tar.gz` of the
//! plugin's folder, as `install` reads it) and its checksum. Flux keeps the last index it read in
//! its cache, installs a package after checking its SHA-256, finds updates of installed plugins and
//! suggests a plugin for a file Flux has no language for ([`suggest`]).
//!
//! `FLUX_CATALOG_URL` reads another index (`file://` or `https://`): a local index for tests and
//! manual checks before the catalog is published.

use std::cmp::Ordering as Order;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use ureq::Agent;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

use crate::API_VERSION;
use crate::manifest::{Manifest, ManifestError};

/// The version of the index's format Flux reads.
pub const INDEX_FORMAT: u32 = 1;

/// Where Flux reads the index from, unless `FLUX_CATALOG_URL` says otherwise. The catalog
/// repository is published with the author's go-ahead (stage 8.3); until then a local index.
pub const DEFAULT_INDEX_URL: &str =
    "https://raw.githubusercontent.com/egor4geev/flux-plugins/main/index.json";

/// The largest index Flux reads.
const MAX_INDEX: u64 = 32 * 1024 * 1024;
/// The largest package Flux downloads.
const MAX_PACKAGE: u64 = 512 * 1024 * 1024;
/// A download reads its body in pieces of this size, looking at the cancel flag between them.
const CHUNK: usize = 64 * 1024;

/// The index Flux reads: `FLUX_CATALOG_URL`, otherwise [`DEFAULT_INDEX_URL`].
pub fn index_url() -> String {
    std::env::var("FLUX_CATALOG_URL")
        .ok()
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| DEFAULT_INDEX_URL.to_string())
}

/// The catalog's index.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Index {
    /// [`INDEX_FORMAT`].
    pub format: u32,
    /// When it was built (ISO 8601).
    #[serde(default)]
    pub generated: Option<String>,
    pub plugins: Vec<IndexEntry>,
}

/// A published plugin: its latest version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    /// The plugin API version (`"0.2"`): Flux installs only the ones it runs.
    pub api: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub categories: Vec<Category>,
    /// The package's `flux-plugin.toml`, as it is: the details and the install question read it.
    pub manifest: String,
    /// The package: a `.tar.gz` of the plugin's folder.
    pub download: String,
    /// The package's SHA-256, in lowercase hex.
    pub sha256: String,
    /// The package's size in bytes.
    pub size: u64,
    /// The plugin's icon (a 16×16 SVG), to show before it is installed.
    #[serde(default)]
    pub icon_svg: Option<String>,
    /// The plugin's README (Markdown).
    #[serde(default)]
    pub readme: Option<String>,
    /// The languages it adds: what [`suggest`] looks at.
    #[serde(default)]
    pub languages: Vec<IndexLanguage>,
    /// The color themes it adds.
    #[serde(default)]
    pub themes: Vec<IndexTheme>,
    /// The names of the sets of file icons it adds.
    #[serde(default)]
    pub icon_themes: Vec<String>,
    /// When this version was published (ISO 8601 date).
    #[serde(default)]
    pub updated: Option<String>,
    /// The plugin's translations of its manifest's strings (`locales/<language>.toml`), by
    /// language: the English text is the key, as everywhere in Flux.
    #[serde(default)]
    pub locales: BTreeMap<String, BTreeMap<String, String>>,
}

/// What kind of plugin it is, for the Marketplace's filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    Languages,
    Themes,
    Icons,
    Tools,
}

impl Category {
    pub const ALL: [Category; 4] = [
        Category::Languages,
        Category::Themes,
        Category::Icons,
        Category::Tools,
    ];
}

/// A language a published plugin adds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexLanguage {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub file_names: Vec<String>,
}

/// A color theme a published plugin adds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexTheme {
    pub name: String,
    /// "dark" or "light".
    pub appearance: String,
}

impl IndexEntry {
    /// The package's manifest.
    pub fn parse_manifest(&self) -> Result<Manifest, ManifestError> {
        Manifest::parse(&self.manifest)
    }

    /// Flux runs this plugin's API version.
    pub fn compatible(&self) -> bool {
        self.api == API_VERSION
    }

    /// A string of the plugin (its name, its description) in `language` ("ru"); the English text
    /// when the plugin has no translation.
    pub fn translate<'a>(&'a self, language: &str, text: &'a str) -> &'a str {
        self.locales
            .get(language)
            .and_then(|strings| strings.get(text))
            .map_or(text, String::as_str)
    }

    /// The language this plugin adds for `path`, if any: by the exact file name first, then by the
    /// extension (in any case).
    pub fn language_for(&self, path: &Path) -> Option<&IndexLanguage> {
        let file_name = path.file_name()?.to_str()?;
        if let Some(language) = self
            .languages
            .iter()
            .find(|language| language.file_names.iter().any(|name| name == file_name))
        {
            return Some(language);
        }
        let extension = path.extension()?.to_str()?;
        self.languages.iter().find(|language| {
            language
                .extensions
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
    }
}

impl Index {
    /// A published plugin by id.
    pub fn plugin(&self, id: &str) -> Option<&IndexEntry> {
        self.plugins.iter().find(|entry| entry.id == id)
    }
}

/// Reads the index at `url` (`https://` or `file://`) and keeps it in the cache. Blocks: call it
/// off the UI thread.
pub fn fetch(url: &str) -> Result<Index, String> {
    let bytes = read_url(url, MAX_INDEX)?;
    let index = parse_index(&bytes)?;
    store_cache(url, &bytes);
    Ok(index)
}

/// Reads an index from its JSON.
pub fn parse_index(bytes: &[u8]) -> Result<Index, String> {
    let index: Index =
        serde_json::from_slice(bytes).map_err(|err| format!("The index can't be read: {err}"))?;
    if index.format > INDEX_FORMAT {
        return Err(format!(
            "The index is in format {}; this Flux reads format {INDEX_FORMAT}",
            index.format
        ));
    }
    Ok(index)
}

/// The index read last time from [`index_url`], from the cache; none before the first [`fetch`].
pub fn cached() -> Option<Index> {
    cached_for(&index_url())
}

/// The index read last time from `url`, from the cache.
pub fn cached_for(url: &str) -> Option<Index> {
    let dir = cache_dir();
    let source = std::fs::read_to_string(dir.join("index.url")).ok()?;
    if source.trim() != url {
        return None;
    }
    let bytes = std::fs::read(dir.join("index.json")).ok()?;
    parse_index(&bytes).ok()
}

/// Keeps the index just read, with the URL it came from (a cache of another URL isn't used).
fn store_cache(url: &str, bytes: &[u8]) {
    let dir = cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let temp = dir.join("index.json.flux-tmp");
    if std::fs::write(&temp, bytes).is_ok() && std::fs::rename(&temp, dir.join("index.json")).is_ok()
    {
        let _ = std::fs::write(dir.join("index.url"), url);
    }
}

/// The cache of the catalog: the last index and downloaded packages.
pub fn cache_dir() -> PathBuf {
    crate::paths::cache_dir().join("catalog")
}

/// Downloads a plugin's package into the catalog's cache and checks its SHA-256; returns the
/// archive, ready for `install::inspect`. `progress` gets (bytes so far, all bytes); `cancel` stops
/// the download. A package already downloaded with the right checksum isn't downloaded again.
/// Blocks: call it off the UI thread.
pub fn download(
    entry: &IndexEntry,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<PathBuf, String> {
    let dir = cache_dir().join("packages");
    std::fs::create_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    let file_name = format!("{}-{}.tar.gz", safe_name(&entry.id), safe_name(&entry.version));
    let target = dir.join(&file_name);
    let expected = entry.sha256.trim().to_ascii_lowercase();
    if let Ok(bytes) = std::fs::read(&target)
        && sha256_hex(&bytes) == expected
    {
        progress(bytes.len() as u64, bytes.len() as u64);
        return Ok(target);
    }
    let bytes = if let Some(path) = entry.download.strip_prefix("file://") {
        let bytes = std::fs::read(file_path(path))
            .map_err(|err| format!("{}: {err}", entry.download))?;
        progress(bytes.len() as u64, bytes.len() as u64);
        bytes
    } else {
        download_http(&entry.download, entry.size, cancel, progress)?
    };
    if cancel.load(Ordering::Relaxed) {
        return Err(CANCELLED.into());
    }
    let actual = sha256_hex(&bytes);
    if actual != expected {
        return Err(format!(
            "The package's checksum doesn't match the catalog's (SHA-256 {actual}, expected \
             {expected}): it wasn't installed"
        ));
    }
    let temp = dir.join(format!("{file_name}.flux-tmp"));
    std::fs::write(&temp, &bytes).map_err(|err| format!("{}: {err}", temp.display()))?;
    std::fs::rename(&temp, &target).map_err(|err| format!("{}: {err}", target.display()))?;
    Ok(target)
}

/// The error of a download stopped by its cancel flag.
pub const CANCELLED: &str = "Cancelled";

/// A file name made of an id or a version: no path separators.
fn safe_name(text: &str) -> String {
    text.chars()
        .map(|c| if c == '/' || c == '\\' || c == ':' { '_' } else { c })
        .collect()
}

/// `file:///Users/me/x` → `/Users/me/x` (percent-escapes decoded).
fn file_path(rest: &str) -> PathBuf {
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    PathBuf::from(percent_decode(path))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&text[i + 1..i + 3], 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Reads a whole `https://`, `http://` or `file://` URL, up to `limit` bytes.
fn read_url(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    if let Some(path) = url.strip_prefix("file://") {
        let path = file_path(path);
        return std::fs::read(&path).map_err(|err| format!("{}: {err}", path.display()));
    }
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(format!("{url}: not an https:// or file:// address"));
    }
    let mut response = agent()
        .get(url)
        .call()
        .map_err(|err| describe(url, err))?;
    response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|err| describe(url, err))
}

/// Downloads `url`, telling the progress; stops when `cancel` is set.
fn download_http(
    url: &str,
    size: u64,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Vec<u8>, String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(format!("{url}: not an https:// or file:// address"));
    }
    let mut response = agent()
        .get(url)
        .call()
        .map_err(|err| describe(url, err))?;
    let total = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(size);
    let mut reader = response
        .body_mut()
        .with_config()
        .limit(MAX_PACKAGE)
        .reader();
    let mut bytes = Vec::with_capacity(total.min(MAX_PACKAGE) as usize);
    let mut buffer = vec![0; CHUNK];
    progress(0, total);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|err| format!("{url}: {err}"))?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        progress(bytes.len() as u64, total.max(bytes.len() as u64));
    }
    Ok(bytes)
}

/// The client of the catalog: redirects followed (GitHub's downloads go to its storage), the
/// system's certificates (a company's root certificates work).
fn agent() -> Agent {
    Agent::config_builder()
        .user_agent(format!("Flux/{}", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::Rustls)
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into()
}

/// A failed request in plain words.
fn describe(url: &str, err: ureq::Error) -> String {
    match err {
        ureq::Error::StatusCode(status) => format!("{url}: the server answered {status}"),
        ureq::Error::HostNotFound => format!("{url}: the server wasn't found (offline?)"),
        ureq::Error::Timeout(_) => format!("{url}: timed out"),
        ureq::Error::BodyExceedsLimit(_) => format!("{url}: too large"),
        other => format!("{url}: {other}"),
    }
}

/// SHA-256 in lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Whether version `candidate` is newer than `current` (`1.10.0` > `1.9.2`; a pre-release is older
/// than its release: `1.0.0-beta.2` < `1.0.0`; build metadata after `+` doesn't count).
pub fn is_newer(candidate: &str, current: &str) -> bool {
    compare_versions(candidate, current) == Order::Greater
}

/// Versions compared as SemVer: the numbers, then the pre-release (none is newest).
pub fn compare_versions(a: &str, b: &str) -> Order {
    let (core_a, pre_a) = split_version(a);
    let (core_b, pre_b) = split_version(b);
    let parts_a: Vec<&str> = core_a.split('.').collect();
    let parts_b: Vec<&str> = core_b.split('.').collect();
    for i in 0..parts_a.len().max(parts_b.len()) {
        let x = parts_a.get(i).copied().unwrap_or("0");
        let y = parts_b.get(i).copied().unwrap_or("0");
        let order = compare_identifiers(x, y);
        if order != Order::Equal {
            return order;
        }
    }
    match (pre_a, pre_b) {
        (None, None) => Order::Equal,
        (None, Some(_)) => Order::Greater,
        (Some(_), None) => Order::Less,
        (Some(x), Some(y)) => {
            let xs: Vec<&str> = x.split('.').collect();
            let ys: Vec<&str> = y.split('.').collect();
            for (x, y) in xs.iter().zip(&ys) {
                let order = compare_identifiers(x, y);
                if order != Order::Equal {
                    return order;
                }
            }
            xs.len().cmp(&ys.len())
        }
    }
}

/// `1.2.3-beta.1+build` → ("1.2.3", Some("beta.1")); a leading `v` is dropped.
fn split_version(version: &str) -> (&str, Option<&str>) {
    let version = version.trim();
    let version = version.strip_prefix('v').unwrap_or(version);
    let version = version.split('+').next().unwrap_or(version);
    match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    }
}

/// Numbers by value (and before words, as SemVer orders pre-release identifiers), words by text.
fn compare_identifiers(a: &str, b: &str) -> Order {
    match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        (Ok(_), Err(_)) => Order::Less,
        (Err(_), Ok(_)) => Order::Greater,
        (Err(_), Err(_)) => a.cmp(b),
    }
}

/// The published plugins that are newer than the installed ones (id, version) and that Flux can
/// run.
pub fn updates<'a>(index: &'a Index, installed: &[(String, String)]) -> Vec<&'a IndexEntry> {
    installed
        .iter()
        .filter_map(|(id, version)| {
            let entry = index.plugin(id)?;
            (entry.compatible() && is_newer(&entry.version, version)).then_some(entry)
        })
        .collect()
}

/// A compatible published plugin that adds a language for `path` (by file name, then by
/// extension): for a file Flux has no language for.
pub fn suggest<'a>(index: &'a Index, path: &Path) -> Option<&'a IndexEntry> {
    let file_name = path.file_name()?.to_str()?;
    let compatible = || index.plugins.iter().filter(|entry| entry.compatible());
    if let Some(entry) = compatible().find(|entry| {
        entry
            .languages
            .iter()
            .any(|language| language.file_names.iter().any(|name| name == file_name))
    }) {
        return Some(entry);
    }
    let extension = path.extension()?.to_str()?;
    compatible().find(|entry| {
        entry.languages.iter().any(|language| {
            language
                .extensions
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
    })
}

/// The kind of file a suggestion is about, as the user ignores it: "*.rs" for an extension a
/// language claims, the file name ("Dockerfile") for a file name.
pub fn suggestion_kind(entry: &IndexEntry, path: &Path) -> Option<String> {
    let file_name = path.file_name()?.to_str()?;
    let by_name = entry
        .languages
        .iter()
        .any(|language| language.file_names.iter().any(|name| name == file_name));
    if by_name {
        return Some(file_name.to_string());
    }
    let extension = path.extension()?.to_str()?;
    Some(format!("*.{}", extension.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::Arc;

    use super::*;
    use crate::tests::temp_dir;

    fn entry(id: &str, version: &str) -> IndexEntry {
        IndexEntry {
            id: id.into(),
            name: id.into(),
            version: version.into(),
            api: API_VERSION.into(),
            description: String::new(),
            authors: Vec::new(),
            repository: None,
            categories: vec![Category::Languages],
            manifest: format!(
                "id = \"{id}\"\nname = \"{id}\"\nversion = \"{version}\"\napi = \"{API_VERSION}\"\n"
            ),
            download: String::new(),
            sha256: String::new(),
            size: 0,
            icon_svg: None,
            readme: None,
            languages: Vec::new(),
            themes: Vec::new(),
            icon_themes: Vec::new(),
            updated: None,
            locales: BTreeMap::new(),
        }
    }

    fn language(id: &str, extensions: &[&str], file_names: &[&str]) -> IndexLanguage {
        IndexLanguage {
            id: id.into(),
            name: id.into(),
            extensions: extensions.iter().map(|s| s.to_string()).collect(),
            file_names: file_names.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn versions_compare_as_semver() {
        assert!(is_newer("1.10.0", "1.9.2"));
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("1.0.0", "1.0.0-beta.2"));
        assert!(is_newer("1.0.0-beta.11", "1.0.0-beta.2"));
        assert!(is_newer("1.0.0-beta", "1.0.0-alpha.9"));
        assert!(is_newer("1.0.0-alpha.1", "1.0.0-alpha"));
        assert!(is_newer("1.0.1", "v1.0.0"));
        assert!(is_newer("1.1", "1.0.9"));
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("1.0.0+build.5", "1.0.0"));
        assert!(!is_newer("1.0", "1.0.0"));
        assert!(!is_newer("0.9.9", "1.0.0"));
        assert_eq!(compare_versions("2.0.0", "10.0.0"), Order::Less);
    }

    #[test]
    fn updates_are_newer_compatible_versions_of_installed_plugins() {
        let mut old_api = entry("flux.old", "2.0.0");
        old_api.api = "0.1".into();
        let index = Index {
            format: INDEX_FORMAT,
            generated: None,
            plugins: vec![
                entry("flux.rust", "0.2.0"),
                entry("flux.go", "0.1.0"),
                old_api,
                entry("flux.notinstalled", "9.9.9"),
            ],
        };
        let installed = [
            ("flux.rust".to_string(), "0.1.0".to_string()),
            ("flux.go".to_string(), "0.1.0".to_string()),
            ("flux.old".to_string(), "1.0.0".to_string()),
        ];
        let found: Vec<&str> = updates(&index, &installed)
            .iter()
            .map(|entry| entry.id.as_str())
            .collect();
        assert_eq!(found, ["flux.rust"]);
    }

    #[test]
    fn suggestions_by_file_name_then_extension() {
        let mut rust = entry("flux.rust", "0.1.0");
        rust.languages = vec![language("rust", &["rs"], &[])];
        let mut docker = entry("flux.dockerfile", "0.1.0");
        docker.languages = vec![language("dockerfile", &["dockerfile"], &["Dockerfile"])];
        let mut future = entry("flux.future", "0.1.0");
        future.api = "9.9".into();
        future.languages = vec![language("zig", &["zig"], &[])];
        let index = Index {
            format: INDEX_FORMAT,
            generated: None,
            plugins: vec![rust, docker, future],
        };
        let id = |path: &str| suggest(&index, Path::new(path)).map(|entry| entry.id.clone());
        assert_eq!(id("src/main.rs").as_deref(), Some("flux.rust"));
        assert_eq!(id("src/MAIN.RS").as_deref(), Some("flux.rust"));
        assert_eq!(id("build/Dockerfile").as_deref(), Some("flux.dockerfile"));
        assert_eq!(id("api.dockerfile").as_deref(), Some("flux.dockerfile"));
        assert_eq!(id("main.zig"), None, "an incompatible plugin isn't suggested");
        assert_eq!(id("notes.txt"), None);
        let docker = index.plugin("flux.dockerfile").unwrap();
        assert_eq!(
            suggestion_kind(docker, Path::new("x/Dockerfile")).as_deref(),
            Some("Dockerfile")
        );
        assert_eq!(
            suggestion_kind(index.plugin("flux.rust").unwrap(), Path::new("a.RS")).as_deref(),
            Some("*.rs")
        );
        assert_eq!(
            docker.language_for(Path::new("Dockerfile")).map(|l| l.id.as_str()),
            Some("dockerfile")
        );
    }

    #[test]
    fn the_index_comes_from_a_file_and_is_cached() {
        let dir = temp_dir("catalog-file");
        let index = Index {
            format: INDEX_FORMAT,
            generated: Some("2026-10-10T12:00:00Z".into()),
            plugins: vec![entry("flux.rust", "0.1.0")],
        };
        let path = dir.join("my index.json");
        std::fs::write(&path, serde_json::to_vec(&index).unwrap()).unwrap();
        let url = format!("file://{}", path.display().to_string().replace(' ', "%20"));
        assert_eq!(fetch(&url).unwrap(), index);
        assert_eq!(cached_for(&url), Some(index.clone()));
        assert_eq!(cached_for("file:///elsewhere/index.json"), None);
        assert!(fetch("file:///no/such/index.json").is_err());
        std::fs::write(&path, b"{\"format\": 99, \"plugins\": []}").unwrap();
        assert!(fetch(&url).unwrap_err().contains("format 99"));
        assert!(fetch("ftp://example.com/index.json").is_err());
    }

    /// A one-shot HTTP server on 127.0.0.1 that answers every request with `body`, in pieces with
    /// a pause between them.
    fn serve(body: Vec<u8>, pieces: usize, pause: Duration) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    return;
                };
                let body = body.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    while reader.read_line(&mut line).is_ok() {
                        if line == "\r\n" || line.is_empty() {
                            break;
                        }
                        line.clear();
                    }
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/gzip\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    if stream.write_all(head.as_bytes()).is_err() {
                        return;
                    }
                    let size = body.len().div_ceil(pieces.max(1)).max(1);
                    for piece in body.chunks(size) {
                        if stream.write_all(piece).is_err() || stream.flush().is_err() {
                            return;
                        }
                        std::thread::sleep(pause);
                    }
                });
            }
        });
        format!("http://{address}/package.tar.gz")
    }

    fn package(id: &str, bytes: &[u8], url: String) -> IndexEntry {
        let mut entry = entry(id, "1.0.0");
        entry.download = url;
        entry.sha256 = sha256_hex(bytes);
        entry.size = bytes.len() as u64;
        entry
    }

    #[test]
    fn a_package_is_downloaded_checked_and_kept() {
        let bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let url = serve(bytes.clone(), 6, Duration::from_millis(5));
        let entry = package("test.download", &bytes, url);
        let mut seen = Vec::new();
        let path = download(&entry, &AtomicBool::new(false), &mut |done, total| {
            seen.push((done, total))
        })
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(seen.len() >= 2, "{seen:?}");
        assert_eq!(seen.last(), Some(&(bytes.len() as u64, bytes.len() as u64)));
        // Already there with the right checksum: not downloaded again.
        let mut again = entry.clone();
        again.download = "http://127.0.0.1:9/nothing-listens".into();
        assert_eq!(
            download(&again, &AtomicBool::new(false), &mut |_, _| {}).unwrap(),
            path
        );
    }

    #[test]
    fn a_package_with_another_checksum_is_refused() {
        let bytes = b"not the package the catalog describes".to_vec();
        let url = serve(bytes.clone(), 1, Duration::ZERO);
        let mut entry = package("test.checksum", &bytes, url);
        entry.sha256 = sha256_hex(b"something else");
        let err = download(&entry, &AtomicBool::new(false), &mut |_, _| {}).unwrap_err();
        assert!(err.contains("checksum"), "{err}");
        let kept = cache_dir().join("packages/test.checksum-1.0.0.tar.gz");
        assert!(!kept.exists());
    }

    #[test]
    fn a_download_stops_when_cancelled() {
        let bytes: Vec<u8> = vec![7; 400_000];
        let url = serve(bytes.clone(), 40, Duration::from_millis(20));
        let entry = package("test.cancel", &bytes, url);
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let err = download(&entry, &cancel, &mut |done, _| {
            if done > 0 {
                flag.store(true, Ordering::Relaxed);
            }
        })
        .unwrap_err();
        assert_eq!(err, CANCELLED);
    }

    #[test]
    fn a_package_from_a_file() {
        let dir = temp_dir("catalog-package-file");
        let bytes = b"package bytes".to_vec();
        let path = dir.join("test.file-1.0.0.tar.gz");
        std::fs::write(&path, &bytes).unwrap();
        let entry = package("test.file", &bytes, format!("file://{}", path.display()));
        let downloaded = download(&entry, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
        assert_eq!(std::fs::read(downloaded).unwrap(), bytes);
    }

    #[test]
    fn strings_are_translated_by_the_plugins_locales() {
        let mut entry = entry("flux.rust", "0.1.0");
        entry.description = "Rust support.".into();
        entry.locales.insert(
            "ru".into(),
            BTreeMap::from([("Rust support.".to_string(), "Поддержка Rust.".to_string())]),
        );
        assert_eq!(entry.translate("ru", "Rust support."), "Поддержка Rust.");
        assert_eq!(entry.translate("en", "Rust support."), "Rust support.");
        assert_eq!(entry.translate("ru", "Other"), "Other");
        let json = serde_json::to_string(&entry).unwrap();
        let back: IndexEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, entry);
    }

    #[test]
    fn checksums_are_lowercase_hex() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
