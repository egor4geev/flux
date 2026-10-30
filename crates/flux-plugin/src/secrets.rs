//! The plugins' secrets (`secrets` interface, Settings' secret fields): tokens and passwords, which
//! Flux keeps itself, encrypted, in a file only the user can read — `secrets.bin` beside the
//! plugins ([`crate::paths::secrets_dir`]), with its key in `secrets.key`. A plugin reads and
//! writes its own, under its service ([`service`]); all of them go when it is uninstalled
//! ([`crate::install::uninstall`]).
//!
//! `secrets.bin`: [`HEADER`] (`FLUXSEC` and the format's version), a random nonce, new at every
//! write, then the JSON of every plugin's secrets (by service, then account) sealed with
//! ChaCha20-Poly1305, the header as associated data. A write goes to a new file renamed over the
//! old one; two Flux processes at once (the everyday one and a build being tried) take turns by a
//! lock on `secrets.lock`. What was read stays in memory until the file changes. A file that can't
//! be read (another key, damaged) is never written over: it is put aside as
//! `secrets.bin.unreadable-<unix time>`, and the secrets start empty — the plugins look signed out.
//!
//! Flux kept them in the macOS keychain before: the first use of the secrets in Flux.app moves them
//! here, once ([`Store::move_from_keychain`]).
//!
//! `FLUX_PLUGIN_SECRETS_FILE=<file>` keeps them in a plain JSON file instead (tests, scenarios).

use std::collections::BTreeMap;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{ErrorKind, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom as _, SystemRandom};

use crate::keychain::{self, Keychain};

/// The secrets of every plugin by service, then account.
type Secrets = BTreeMap<String, BTreeMap<String, String>>;

/// What a plugin's service starts with.
pub(crate) const SERVICE_PREFIX: &str = "Flux plugin ";

/// The service of a plugin's secrets (their keychain items' service before).
pub fn service(plugin: &str) -> String {
    format!("{SERVICE_PREFIX}{plugin}")
}

pub fn get(service: &str, account: &str) -> Result<Option<String>, String> {
    Place::current().get(service, account)
}

pub fn set(service: &str, account: &str, secret: &str) -> Result<(), String> {
    Place::current().set(service, account, secret)
}

pub fn delete(service: &str, account: &str) -> Result<(), String> {
    Place::current().delete(service, account)
}

/// Deletes every secret of the service, with its items left in the keychain.
pub fn delete_all(service: &str) -> Result<(), String> {
    Place::current().delete_all(service)
}

/// The store's own entry, which no plugin's service can be: when the keychain's secrets were moved
/// ([`MOVED`]). Never read, written or removed as a secret.
const META: &str = "flux";
const MOVED: &str = "moved-from-keychain";

/// Where the secrets are kept.
enum Place<'a> {
    Store(&'a Store),
    /// `FLUX_PLUGIN_SECRETS_FILE`.
    PlainFile(PathBuf),
}

impl Place<'_> {
    fn current() -> Self {
        let file = std::env::var_os("FLUX_PLUGIN_SECRETS_FILE").filter(|file| !file.is_empty());
        match file {
            Some(file) => Place::PlainFile(file.into()),
            None => Place::Store(Store::user()),
        }
    }

    fn change<T>(&self, change: impl FnOnce(&mut Secrets) -> T) -> Result<T, String> {
        match self {
            Place::Store(store) => store.change(change),
            Place::PlainFile(file) => with_plain_file(file, change),
        }
    }

    fn get(&self, service: &str, account: &str) -> Result<Option<String>, String> {
        if service == META {
            return Ok(None);
        }
        self.change(|secrets| {
            secrets
                .get(service)
                .and_then(|accounts| accounts.get(account))
                .cloned()
        })
    }

    fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), String> {
        if service == META {
            return Err("not a plugin's secret".into());
        }
        self.change(|secrets| {
            secrets
                .entry(service.to_string())
                .or_default()
                .insert(account.to_string(), secret.to_string());
        })
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), String> {
        if service == META {
            return Ok(());
        }
        self.change(|secrets| {
            if let Some(accounts) = secrets.get_mut(service) {
                accounts.remove(account);
            }
        })
    }

    fn delete_all(&self, service: &str) -> Result<(), String> {
        if service == META {
            return Ok(());
        }
        let removed = self.change(|secrets| {
            secrets.remove(service);
        });
        // Items the user didn't let Flux move from the keychain.
        let left = match self {
            Place::Store(Store {
                keychain: Some(keychain),
                ..
            }) => keychain.delete_all(service),
            _ => Ok(()),
        };
        removed.and(left)
    }
}

/// Reads and changes the plain secrets file under a lock (the plugins' threads share it).
fn with_plain_file<T>(file: &Path, change: impl FnOnce(&mut Secrets) -> T) -> Result<T, String> {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut secrets: Secrets = match fs::read(file) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|err| err.to_string())?,
        Err(err) if err.kind() == ErrorKind::NotFound => Secrets::new(),
        Err(err) => return Err(err.to_string()),
    };
    let before = secrets.clone();
    let result = change(&mut secrets);
    if secrets != before {
        let text = serde_json::to_vec_pretty(&secrets).map_err(|err| err.to_string())?;
        fs::write(file, text).map_err(|err| err.to_string())?;
    }
    Ok(result)
}

/// `secrets.bin`'s header: the format and its version. Sealed along with the secrets.
const HEADER: [u8; 8] = *b"FLUXSEC\x01";
/// The format's name: the header without the version.
const MAGIC: &[u8] = b"FLUXSEC";

const BIN: &str = "secrets.bin";
const KEY: &str = "secrets.key";
const LOCK: &str = "secrets.lock";
/// A write in progress, renamed to [`BIN`] when complete.
const NEW: &str = "secrets.bin.new";

const KEY_LEN: usize = 32;
type Key = [u8; KEY_LEN];

/// The encrypted secrets in a folder.
struct Store {
    dir: PathBuf,
    /// Where the secrets were before, to move them from: none in tests and scenarios.
    keychain: Option<Arc<dyn Keychain>>,
    /// The plugins' threads take turns (other processes — by the lock file).
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// The secrets as last read or written.
    cache: Option<Cached>,
    /// The keychain was looked at: once a process, so a denied read isn't asked again.
    keychain_checked: bool,
}

#[derive(Default)]
struct Cached {
    /// Which `secrets.bin` they are; none when there is none.
    stamp: Option<Stamp>,
    secrets: Secrets,
}

/// A version of `secrets.bin`: every write makes a new file.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    inode: u64,
}

impl Stamp {
    fn of(meta: &Metadata) -> Self {
        #[cfg(unix)]
        let inode = std::os::unix::fs::MetadataExt::ino(meta);
        #[cfg(not(unix))]
        let inode = 0;
        Self {
            len: meta.len(),
            modified: meta.modified().ok(),
            inode,
        }
    }
}

/// Why `secrets.bin` can't be read.
enum Unreadable {
    /// Another key, damaged, not the secrets at all: put aside.
    Damaged(String),
    /// Saved by a newer Flux, or the key can't be read now: left as it is.
    Error(String),
}

impl Store {
    fn new(dir: PathBuf, keychain: Option<Arc<dyn Keychain>>) -> Self {
        Self {
            dir,
            keychain,
            state: Mutex::default(),
        }
    }

    /// The user's secrets (or those of `FLUX_PLUGINS_HOME`).
    fn user() -> &'static Store {
        static STORE: OnceLock<Store> = OnceLock::new();
        STORE.get_or_init(|| Store::new(crate::paths::secrets_dir(), keychain::user()))
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// Reads and changes the secrets, writing them if they changed. The first time, moves the
    /// keychain's secrets here.
    fn change<T>(&self, change: impl FnOnce(&mut Secrets) -> T) -> Result<T, String> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let _lock = self.lock()?;
        if !state.keychain_checked {
            state.keychain_checked = true;
            if let Some(keychain) = &self.keychain
                && let Err(err) = self.move_from_keychain(&mut state, keychain.as_ref())
            {
                log::warn!("The plugins' secrets weren't moved from the keychain: {err}");
            }
        }
        let cached = self.refresh(&mut state)?;
        let before = cached.secrets.clone();
        let result = change(&mut cached.secrets);
        if cached.secrets != before {
            match self.save(&cached.secrets) {
                Ok(stamp) => cached.stamp = Some(stamp),
                Err(err) => {
                    state.cache = None;
                    return Err(err);
                }
            }
        }
        Ok(result)
    }

    /// Takes the lock other Flux processes take (released when the file is dropped). Makes the
    /// folder, the user's alone, if there is none.
    fn lock(&self) -> Result<File, String> {
        let mut folder = fs::DirBuilder::new();
        folder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut folder, 0o700);
        folder
            .create(&self.dir)
            .map_err(|err| format!("{}: {err}", self.dir.display()))?;
        let path = self.path(LOCK);
        let file = private()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|err| format!("{}: {err}", path.display()))?;
        file.lock()
            .map_err(|err| format!("{}: {err}", path.display()))?;
        Ok(file)
    }

    /// The secrets in `secrets.bin`, read again only when the file changed.
    fn refresh<'a>(&self, state: &'a mut State) -> Result<&'a mut Cached, String> {
        let path = self.path(BIN);
        match File::open(&path) {
            Ok(mut file) => {
                let stamp = file
                    .metadata()
                    .map(|meta| Stamp::of(&meta))
                    .map_err(|err| format!("{}: {err}", path.display()))?;
                let fresh = state
                    .cache
                    .as_ref()
                    .is_some_and(|cached| cached.stamp == Some(stamp));
                if !fresh {
                    let mut bytes = Vec::new();
                    file.read_to_end(&mut bytes)
                        .map_err(|err| format!("{}: {err}", path.display()))?;
                    state.cache = Some(match self.open(&bytes) {
                        Ok(secrets) => Cached {
                            stamp: Some(stamp),
                            secrets,
                        },
                        Err(Unreadable::Damaged(why)) => {
                            let aside = self.put_aside(BIN)?;
                            log::warn!(
                                "The plugins' secrets can't be read ({why}): put aside as {}; \
                                 starting with none",
                                aside.display()
                            );
                            Cached::default()
                        }
                        Err(Unreadable::Error(err)) => return Err(err),
                    });
                }
            }
            Err(err) if err.kind() == ErrorKind::NotFound => state.cache = None,
            Err(err) => return Err(format!("{}: {err}", path.display())),
        }
        Ok(state.cache.get_or_insert_default())
    }

    /// The secrets in `secrets.bin`'s bytes.
    fn open(&self, bytes: &[u8]) -> Result<Secrets, Unreadable> {
        let damaged = |why: &str| Unreadable::Damaged(why.to_string());
        let rest = bytes
            .strip_prefix(MAGIC)
            .ok_or_else(|| damaged("not a secrets file"))?;
        let (&version, sealed) = rest.split_first().ok_or_else(|| damaged("cut short"))?;
        if version > HEADER[MAGIC.len()] {
            return Err(Unreadable::Error(
                "The plugins' secrets were saved by a newer Flux".into(),
            ));
        }
        if version != HEADER[MAGIC.len()] || sealed.len() < NONCE_LEN {
            return Err(damaged("an unknown format"));
        }
        let key = self
            .key()
            .map_err(Unreadable::Error)?
            .ok_or_else(|| damaged("its key is missing"))?;
        let (nonce, sealed) = sealed.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| damaged("cut short"))?;
        let mut sealed = sealed.to_vec();
        let json = cipher(&key)
            .open_in_place(nonce, Aad::from(HEADER), &mut sealed)
            .map_err(|_| damaged("another key, or damaged"))?;
        serde_json::from_slice(json).map_err(|err| damaged(&err.to_string()))
    }

    /// Writes the secrets sealed with a new nonce: into a new file, renamed over the old one.
    fn save(&self, secrets: &Secrets) -> Result<Stamp, String> {
        let key = self.key_or_new()?;
        let mut sealed = serde_json::to_vec(secrets).map_err(|err| err.to_string())?;
        let mut nonce = [0; NONCE_LEN];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| "No random numbers for the secrets' nonce")?;
        cipher(&key)
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(HEADER),
                &mut sealed,
            )
            .map_err(|_| "Couldn't encrypt the secrets")?;
        let new = self.path(NEW);
        let _ = fs::remove_file(&new);
        let written = private()
            .write(true)
            .create_new(true)
            .open(&new)
            .and_then(|mut file| {
                file.write_all(&HEADER)?;
                file.write_all(&nonce)?;
                file.write_all(&sealed)?;
                file.sync_all()?;
                file.metadata()
            })
            .and_then(|meta| fs::rename(&new, self.path(BIN)).map(|()| Stamp::of(&meta)));
        written.map_err(|err| {
            let _ = fs::remove_file(&new);
            format!("{}: {err}", self.path(BIN).display())
        })
    }

    /// The key of `secrets.bin`; none before the first write. A damaged key file is put aside.
    fn key(&self) -> Result<Option<Key>, String> {
        let path = self.path(KEY);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(format!("{}: {err}", path.display())),
        };
        if let Some(key) = key_file::decode(bytes) {
            return Ok(Some(key));
        }
        let aside = self.put_aside(KEY)?;
        log::warn!(
            "The key of the plugins' secrets is damaged: put aside as {}",
            aside.display()
        );
        Ok(None)
    }

    /// The key, made by the first write: random bytes in a file only the user can read.
    fn key_or_new(&self) -> Result<Key, String> {
        if let Some(key) = self.key()? {
            return Ok(key);
        }
        let mut key = [0; KEY_LEN];
        SystemRandom::new()
            .fill(&mut key)
            .map_err(|_| "No random numbers for the secrets' key")?;
        let path = self.path(KEY);
        let sealed = key_file::encode(&key).ok_or("The system can't protect the secrets' key")?;
        let created = private().write(true).create_new(true).open(&path);
        match created {
            Ok(mut file) => {
                file.write_all(&sealed)
                    .and_then(|()| file.sync_all())
                    .map_err(|err| format!("{}: {err}", path.display()))?;
                Ok(key)
            }
            // Another Flux made it first: its key it is.
            Err(err) if err.kind() == ErrorKind::AlreadyExists => self
                .key()?
                .ok_or_else(|| format!("{}: can't be read", path.display())),
            Err(err) => Err(format!("{}: {err}", path.display())),
        }
    }

    /// Renames a file that can't be read to `<name>.unreadable-<unix time>`: never written over.
    fn put_aside(&self, name: &str) -> Result<PathBuf, String> {
        let time = unix_time();
        let aside = (0..100)
            .map(|n| match n {
                0 => self.path(&format!("{name}.unreadable-{time}")),
                n => self.path(&format!("{name}.unreadable-{time}-{n}")),
            })
            .find(|path| !path.exists())
            .ok_or_else(|| format!("{name} can't be read nor put aside"))?;
        fs::rename(self.path(name), &aside)
            .map_err(|err| format!("{name} can't be read nor put aside: {err}"))?;
        Ok(aside)
    }

    /// Moves the secrets the keychain kept before here, once: the secrets remember it ([`MOVED`]).
    /// An item is deleted from the keychain only once written here; one whose read fails (the user
    /// denied it) stays there and isn't asked for again — its plugin looks signed out.
    fn move_from_keychain(&self, state: &mut State, keychain: &dyn Keychain) -> Result<(), String> {
        let cached = self.refresh(state)?;
        let moved_before = cached
            .secrets
            .get(META)
            .is_some_and(|meta| meta.contains_key(MOVED));
        if moved_before {
            return Ok(());
        }
        // A failed list isn't remembered: the next start tries again.
        let items = keychain.list()?;
        let mut secrets = cached.secrets.clone();
        let mut moved = Vec::new();
        for (service, account) in items {
            match keychain.read(&service, &account) {
                Ok(Some(secret)) => {
                    // One saved here already is newer.
                    secrets
                        .entry(service.clone())
                        .or_default()
                        .entry(account.clone())
                        .or_insert(secret);
                    moved.push((service, account));
                }
                Ok(None) => {}
                Err(err) => {
                    log::warn!("The secret \"{account}\" of {service} stays in the keychain: {err}")
                }
            }
        }
        secrets
            .entry(META.to_string())
            .or_default()
            .insert(MOVED.to_string(), unix_time().to_string());
        match self.save(&secrets) {
            Ok(stamp) => {
                state.cache = Some(Cached {
                    stamp: Some(stamp),
                    secrets,
                })
            }
            Err(err) => {
                state.cache = None;
                return Err(err);
            }
        }
        for (service, account) in &moved {
            if let Err(err) = keychain.delete(service, account) {
                log::warn!("The secret \"{account}\" of {service} is left in the keychain: {err}");
            }
        }
        if !moved.is_empty() {
            log::info!("Moved {} plugins' secrets from the keychain", moved.len());
        }
        Ok(())
    }
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs())
}

/// The cipher of a key.
fn cipher(key: &Key) -> LessSafeKey {
    LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, key).expect("a key of 32 bytes"))
}

/// Options of a file only the user can read and write (0600).
fn private() -> OpenOptions {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
}

/// The key as its file holds it. macOS, Linux: as it is — the file is the user's alone.
#[cfg(not(windows))]
mod key_file {
    use super::Key;

    pub fn encode(key: &Key) -> Option<Vec<u8>> {
        Some(key.to_vec())
    }

    pub fn decode(bytes: Vec<u8>) -> Option<Key> {
        bytes.try_into().ok()
    }
}

/// Windows: sealed by DPAPI with the user's own protection (`CryptProtectData`) — only this user
/// on this computer opens it, without a prompt. A file's permissions there aren't the owner-only
/// mode set elsewhere: a plain key would be open to more than the user.
#[cfg(windows)]
mod key_file {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    use super::Key;

    /// Mixed into the seal: a blob some other program sealed for the user doesn't open as the key.
    const ENTROPY: &[u8] = b"Flux plugin secrets key";

    pub fn encode(key: &Key) -> Option<Vec<u8>> {
        let mut sealed = CRYPT_INTEGER_BLOB::default();
        // SAFETY: the input blobs point into live slices DPAPI only reads; it allocates `sealed`.
        let done = unsafe {
            CryptProtectData(
                &blob(key),
                std::ptr::null(),
                &blob(ENTROPY),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut sealed,
            )
        };
        (done != 0).then(|| take(sealed))
    }

    pub fn decode(bytes: Vec<u8>) -> Option<Key> {
        let mut plain = CRYPT_INTEGER_BLOB::default();
        // SAFETY: as in `encode`.
        let done = unsafe {
            CryptUnprotectData(
                &blob(&bytes),
                std::ptr::null_mut(),
                &blob(ENTROPY),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut plain,
            )
        };
        if done == 0 {
            return None;
        }
        let mut plain = take(plain);
        let key = plain.as_slice().try_into().ok();
        plain.fill(0);
        key
    }

    fn blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: bytes.len() as u32,
            pbData: bytes.as_ptr().cast_mut(),
        }
    }

    /// The bytes DPAPI returned; its buffer is freed.
    fn take(blob: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        // SAFETY: on success DPAPI hands over `cbData` bytes at `pbData`, allocated with
        // `LocalAlloc`, which are ours to free.
        unsafe {
            let bytes = std::slice::from_raw_parts(blob.pbData, blob.cbData as usize).to_vec();
            LocalFree(blob.pbData.cast());
            bytes
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;
    use std::process::{Command, Stdio};

    use super::*;
    use crate::tests::temp_dir;

    /// A keychain of the test's own: the user's is never touched.
    #[derive(Default)]
    struct FakeKeychain {
        items: Mutex<BTreeMap<(String, String), String>>,
        /// Items whose read the user denies.
        denied: BTreeSet<(String, String)>,
        lists: Mutex<usize>,
    }

    impl FakeKeychain {
        fn with(items: &[(&str, &str, &str)], denied: &[(&str, &str)]) -> Arc<Self> {
            let pair = |service: &str, account: &str| (service.to_string(), account.to_string());
            Arc::new(Self {
                items: Mutex::new(
                    items
                        .iter()
                        .map(|(service, account, secret)| {
                            (pair(service, account), secret.to_string())
                        })
                        .collect(),
                ),
                denied: denied
                    .iter()
                    .map(|(service, account)| pair(service, account))
                    .collect(),
                lists: Mutex::new(0),
            })
        }

        fn left(&self) -> Vec<(String, String)> {
            self.items.lock().unwrap().keys().cloned().collect()
        }
    }

    impl Keychain for FakeKeychain {
        fn list(&self) -> Result<Vec<(String, String)>, String> {
            *self.lists.lock().unwrap() += 1;
            Ok(self.left())
        }

        fn read(&self, service: &str, account: &str) -> Result<Option<String>, String> {
            let item = (service.to_string(), account.to_string());
            if self.denied.contains(&item) {
                return Err("denied".into());
            }
            Ok(self.items.lock().unwrap().get(&item).cloned())
        }

        fn delete(&self, service: &str, account: &str) -> Result<(), String> {
            let item = (service.to_string(), account.to_string());
            self.items.lock().unwrap().remove(&item);
            Ok(())
        }

        fn delete_all(&self, service: &str) -> Result<(), String> {
            self.items
                .lock()
                .unwrap()
                .retain(|(item_service, _), _| item_service != service);
            Ok(())
        }
    }

    fn store(dir: &Path) -> Store {
        Store::new(dir.to_path_buf(), None)
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// The names of the folder's files.
    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn secrets_are_kept_encrypted_and_read_back() {
        let dir = temp_dir("secrets-round-trip");
        let place = Place::Store(&store(&dir));
        assert_eq!(place.get("Flux plugin a", "token").unwrap(), None);
        // Reading creates nothing but the lock.
        assert_eq!(files(&dir), ["secrets.lock"]);
        place.set("Flux plugin a", "token", "s3cret").unwrap();
        place.set("Flux plugin a", "refresh", "r").unwrap();
        place.set("Flux plugin b", "token", "other").unwrap();
        assert_eq!(files(&dir), ["secrets.bin", "secrets.key", "secrets.lock"]);
        let bytes = fs::read(dir.join(BIN)).unwrap();
        assert!(!bytes.windows(6).any(|window| window == b"s3cret"));
        // Another Flux reads them.
        let again = store(&dir);
        let again = Place::Store(&again);
        assert_eq!(
            again.get("Flux plugin a", "token").unwrap().as_deref(),
            Some("s3cret")
        );
        again.delete("Flux plugin a", "token").unwrap();
        assert_eq!(place.get("Flux plugin a", "token").unwrap(), None);
        assert_eq!(
            place.get("Flux plugin a", "refresh").unwrap().as_deref(),
            Some("r")
        );
        place.delete_all("Flux plugin a").unwrap();
        assert_eq!(again.get("Flux plugin a", "refresh").unwrap(), None);
        assert_eq!(
            again.get("Flux plugin b", "token").unwrap().as_deref(),
            Some("other")
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_files_are_the_users_alone() {
        let dir = temp_dir("secrets-modes").join("flux");
        Place::Store(&store(&dir))
            .set("Flux plugin a", "token", "x")
            .unwrap();
        assert_eq!(mode(&dir), 0o700);
        for name in [BIN, KEY, LOCK] {
            assert_eq!(mode(&dir.join(name)), 0o600, "{name}");
        }
        assert_eq!(fs::read(dir.join(KEY)).unwrap().len(), KEY_LEN);
    }

    #[cfg(windows)]
    #[test]
    fn the_key_is_sealed_for_the_user() {
        let dir = temp_dir("secrets-sealed").join("flux");
        Place::Store(&store(&dir))
            .set("Flux plugin a", "token", "x")
            .unwrap();
        let sealed = fs::read(dir.join(KEY)).unwrap();
        assert_ne!(sealed.len(), KEY_LEN);
        assert!(key_file::decode(sealed).is_some());
        assert!(key_file::decode(vec![0; KEY_LEN]).is_none());
    }

    #[test]
    fn every_write_has_a_new_nonce() {
        let dir = temp_dir("secrets-nonce");
        let place = Place::Store(&store(&dir));
        let mut nonces = BTreeSet::new();
        for n in 0..5 {
            place.set("Flux plugin a", "token", &n.to_string()).unwrap();
            let bytes = fs::read(dir.join(BIN)).unwrap();
            assert_eq!(bytes[..HEADER.len()], HEADER);
            nonces.insert(bytes[HEADER.len()..HEADER.len() + NONCE_LEN].to_vec());
        }
        assert_eq!(nonces.len(), 5);
        // The same value again isn't a write.
        let before = fs::read(dir.join(BIN)).unwrap();
        place.set("Flux plugin a", "token", "4").unwrap();
        assert_eq!(fs::read(dir.join(BIN)).unwrap(), before);
    }

    /// Writes a secret, damages the store as told, and checks a new reader starts empty with the
    /// damaged file put aside — and can write again.
    fn damaged(name: &str, damage: impl FnOnce(&Path)) -> PathBuf {
        let dir = temp_dir(name);
        Place::Store(&store(&dir))
            .set("Flux plugin a", "token", "x")
            .unwrap();
        damage(&dir);
        let sealed = fs::read(dir.join(BIN)).unwrap();
        let reader = store(&dir);
        let place = Place::Store(&reader);
        assert_eq!(place.get("Flux plugin a", "token").unwrap(), None);
        let aside: Vec<String> = files(&dir)
            .into_iter()
            .filter(|name| name.starts_with("secrets.bin.unreadable-"))
            .collect();
        assert_eq!(aside.len(), 1, "{:?}", files(&dir));
        assert_eq!(fs::read(dir.join(&aside[0])).unwrap(), sealed);
        assert!(!dir.join(BIN).exists());
        place.set("Flux plugin a", "token", "y").unwrap();
        assert_eq!(
            Place::Store(&store(&dir))
                .get("Flux plugin a", "token")
                .unwrap()
                .as_deref(),
            Some("y")
        );
        dir
    }

    #[test]
    fn an_unreadable_store_is_put_aside_not_written_over() {
        // Another key.
        damaged("secrets-other-key", |dir| {
            fs::write(dir.join(KEY), [7; KEY_LEN]).unwrap();
        });
        // A damaged file.
        damaged("secrets-damaged", |dir| {
            let mut bytes = fs::read(dir.join(BIN)).unwrap();
            *bytes.last_mut().unwrap() ^= 1;
            fs::write(dir.join(BIN), bytes).unwrap();
        });
        // The key lost.
        damaged("secrets-no-key", |dir| {
            fs::remove_file(dir.join(KEY)).unwrap()
        });
        // A damaged key: put aside too, and a new one made.
        let dir = damaged("secrets-damaged-key", |dir| {
            fs::write(dir.join(KEY), b"short").unwrap();
        });
        assert!(
            files(&dir)
                .iter()
                .any(|name| name.starts_with("secrets.key.unreadable-"))
        );
        assert_eq!(fs::read(dir.join(KEY)).unwrap().len(), KEY_LEN);
    }

    #[test]
    fn a_newer_format_is_left_alone() {
        let dir = temp_dir("secrets-newer");
        Place::Store(&store(&dir))
            .set("Flux plugin a", "token", "x")
            .unwrap();
        let mut bytes = fs::read(dir.join(BIN)).unwrap();
        bytes[MAGIC.len()] += 1;
        fs::write(dir.join(BIN), &bytes).unwrap();
        let reader = store(&dir);
        let place = Place::Store(&reader);
        assert!(
            place
                .get("Flux plugin a", "token")
                .unwrap_err()
                .contains("newer Flux")
        );
        assert!(place.set("Flux plugin a", "token", "y").is_err());
        assert_eq!(fs::read(dir.join(BIN)).unwrap(), bytes);
    }

    #[test]
    fn writers_take_turns() {
        let dir = temp_dir("secrets-threads");
        // Two stores on one folder are two processes: only the lock file keeps them apart.
        let stores = [store(&dir), store(&dir)];
        std::thread::scope(|scope| {
            for writer in 0..4 {
                let store = &stores[writer % 2];
                scope.spawn(move || {
                    let place = Place::Store(store);
                    for n in 0..10 {
                        place
                            .set(&format!("Flux plugin w{writer}"), &n.to_string(), "x")
                            .unwrap();
                    }
                });
            }
        });
        let secrets = store(&dir).change(|secrets| secrets.clone()).unwrap();
        assert_eq!(secrets.len(), 4);
        assert!(secrets.values().all(|accounts| accounts.len() == 10));
    }

    /// Run again as a helper process (by the test below): writes its entries and exits.
    const WRITER: &str = "FLUX_SECRETS_TEST_WRITER";

    #[test]
    fn writers_in_two_processes_keep_every_entry() {
        if let Some(dir) = std::env::var_os(WRITER) {
            let store = store(Path::new(&dir));
            let place = Place::Store(&store);
            let service = format!("Flux plugin p{}", std::process::id());
            for n in 0..10 {
                place.set(&service, &n.to_string(), "x").unwrap();
            }
            return;
        }
        let dir = temp_dir("secrets-processes");
        let exe = std::env::current_exe().unwrap();
        let writers: Vec<_> = (0..2)
            .map(|_| {
                Command::new(&exe)
                    .args([
                        "--exact",
                        "secrets::tests::writers_in_two_processes_keep_every_entry",
                        "--test-threads=1",
                    ])
                    .env(WRITER, &dir)
                    .stdout(Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect();
        for mut writer in writers {
            assert!(writer.wait().unwrap().success());
        }
        let secrets = store(&dir).change(|secrets| secrets.clone()).unwrap();
        assert_eq!(secrets.len(), 2, "{:?}", secrets.keys());
        assert!(secrets.values().all(|accounts| accounts.len() == 10));
    }

    #[test]
    fn the_keychain_is_moved_once() {
        let dir = temp_dir("secrets-move");
        let keychain = FakeKeychain::with(
            &[
                ("Flux plugin a", "token", "a1"),
                ("Flux plugin a", "refresh", "a2"),
                ("Flux plugin b", "token", "b1"),
                ("Flux plugin c", "token", "c1"),
                ("Flux plugin d", "token", "old"),
            ],
            &[("Flux plugin c", "token")],
        );
        // A secret saved here already (by a Flux that didn't move them) is newer.
        Place::Store(&store(&dir))
            .set("Flux plugin d", "token", "new")
            .unwrap();
        let mover = Store::new(dir.clone(), Some(keychain.clone()));
        let place = Place::Store(&mover);
        assert_eq!(
            place.get("Flux plugin a", "token").unwrap().as_deref(),
            Some("a1")
        );
        assert_eq!(
            place.get("Flux plugin a", "refresh").unwrap().as_deref(),
            Some("a2")
        );
        assert_eq!(
            place.get("Flux plugin b", "token").unwrap().as_deref(),
            Some("b1")
        );
        assert_eq!(
            place.get("Flux plugin d", "token").unwrap().as_deref(),
            Some("new")
        );
        // The denied one stays in the keychain, the plugin signed out.
        assert_eq!(place.get("Flux plugin c", "token").unwrap(), None);
        assert_eq!(
            keychain.left(),
            [("Flux plugin c".to_string(), "token".to_string())]
        );
        // The mark is no plugin's secret.
        assert_eq!(place.get(META, MOVED).unwrap(), None);
        assert!(place.set(META, MOVED, "x").is_err());
        place.delete_all(META).unwrap();
        // Once: neither this process nor the next one asks again.
        assert_eq!(*keychain.lists.lock().unwrap(), 1);
        let next = Store::new(dir.clone(), Some(keychain.clone()));
        assert_eq!(
            Place::Store(&next)
                .get("Flux plugin b", "token")
                .unwrap()
                .as_deref(),
            Some("b1")
        );
        assert_eq!(*keychain.lists.lock().unwrap(), 1);
        // Uninstalling a plugin removes what the keychain still has of it too.
        Place::Store(&next).delete_all("Flux plugin c").unwrap();
        assert!(keychain.left().is_empty());
        Place::Store(&next).delete_all("Flux plugin a").unwrap();
        assert_eq!(place.get("Flux plugin a", "refresh").unwrap(), None);
    }

    #[test]
    fn a_keychain_that_cant_be_listed_is_tried_again_next_time() {
        struct Locked;
        impl Keychain for Locked {
            fn list(&self) -> Result<Vec<(String, String)>, String> {
                Err("locked".into())
            }
            fn read(&self, _: &str, _: &str) -> Result<Option<String>, String> {
                unreachable!()
            }
            fn delete(&self, _: &str, _: &str) -> Result<(), String> {
                unreachable!()
            }
            fn delete_all(&self, _: &str) -> Result<(), String> {
                Ok(())
            }
        }
        let dir = temp_dir("secrets-locked");
        let locked = Store::new(dir.clone(), Some(Arc::new(Locked)));
        Place::Store(&locked)
            .set("Flux plugin a", "token", "x")
            .unwrap();
        let keychain = FakeKeychain::with(&[("Flux plugin b", "token", "b1")], &[]);
        let next = Store::new(dir, Some(keychain.clone()));
        assert_eq!(
            Place::Store(&next)
                .get("Flux plugin b", "token")
                .unwrap()
                .as_deref(),
            Some("b1")
        );
        assert!(keychain.left().is_empty());
    }

    #[test]
    fn a_plain_file_is_kept_as_before() {
        let file = temp_dir("secrets-plain").join("secrets.json");
        let place = Place::PlainFile(file.clone());
        place.set("Flux plugin a", "token", "s3cret").unwrap();
        place.set("Flux plugin b", "token", "x").unwrap();
        let json: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "Flux plugin a": {"token": "s3cret"},
                "Flux plugin b": {"token": "x"},
            })
        );
        assert_eq!(
            place.get("Flux plugin a", "token").unwrap().as_deref(),
            Some("s3cret")
        );
        place.delete_all("Flux plugin a").unwrap();
        assert_eq!(place.get("Flux plugin a", "token").unwrap(), None);
        assert_eq!(files(file.parent().unwrap()), ["secrets.json"]);
    }
}
