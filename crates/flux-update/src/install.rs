//! Installing a downloaded update (macOS):
//!
//! 1. [`prepare`]: unpack the archive (`ditto -x -k`) next to it, check that it holds `Flux.app`
//!    of the manifest's version, and that its code signature is valid and satisfies the running
//!    app's designated requirement (`codesign --verify --deep --strict -R=<requirement>`): the same
//!    identifier and certificate — the Keychain keeps «Always Allow» of plugin secrets.
//! 2. [`schedule`]: after Flux quits, a helper (`/bin/sh` script, detached) waits for Flux's
//!    process to exit, moves the old bundle to the Trash, puts the new one in its place and opens
//!    it ([`relaunch`] arguments: the projects reopen as on any start).
//!
//! [`can_install`] says whether Flux can replace itself: it runs from a `.app` bundle whose folder
//! it can write to (`/Applications` for an admin user); otherwise the UI opens the release page.
//!
//! The helper logs every step to `~/Library/Logs/Flux/update.log`. `FLUX_UPDATE_NO_RELAUNCH=1` in
//! Flux's environment: the helper swaps the bundles but doesn't open the new one (tests).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// An update unpacked and checked, waiting for Flux to quit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    /// The new `Flux.app`, unpacked into the cache.
    pub app: PathBuf,
    /// The running `Flux.app` it replaces.
    pub target: PathBuf,
    pub version: crate::Version,
}

/// Why Flux can't replace itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CannotInstall {
    /// Not running from a `.app` bundle (`cargo run`, a bare binary).
    NotBundled,
    /// The bundle's folder isn't writable by this user.
    ReadOnly(PathBuf),
    /// The bundle is on a read-only volume or translocated (run from the downloaded `.dmg`).
    Translocated,
}

/// The `Flux.app` this process runs from.
pub fn current_bundle() -> Option<PathBuf> {
    bundle_of(&std::env::current_exe().ok()?)
}

/// The `*.app` whose `Contents/MacOS` holds `exe`.
fn bundle_of(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .skip(1)
        .find(|dir| {
            dir.extension().is_some_and(|ext| ext == "app")
                && exe.starts_with(dir.join("Contents/MacOS"))
        })
        .map(Path::to_path_buf)
}

/// Whether Flux can replace its bundle with an update.
pub fn can_install() -> Result<PathBuf, CannotInstall> {
    let bundle = current_bundle().ok_or(CannotInstall::NotBundled)?;
    check_installable(&bundle)?;
    Ok(bundle)
}

/// Whether `bundle` can be swapped: not translocated, not on a read-only volume, its folder and
/// itself writable (moving a folder elsewhere rewrites its `..`).
pub fn check_installable(bundle: &Path) -> Result<(), CannotInstall> {
    if bundle.to_string_lossy().contains("/AppTranslocation/") || read_only_volume(bundle) {
        return Err(CannotInstall::Translocated);
    }
    let parent = bundle.parent().unwrap_or(Path::new("/"));
    if !writable(parent) || !writable(bundle) {
        return Err(CannotInstall::ReadOnly(parent.to_path_buf()));
    }
    Ok(())
}

#[cfg(unix)]
fn c_path(path: &Path) -> Option<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes()).ok()
}

#[cfg(unix)]
fn writable(path: &Path) -> bool {
    c_path(path).is_some_and(|path| unsafe { libc::access(path.as_ptr(), libc::W_OK) } == 0)
}

#[cfg(not(unix))]
fn writable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| !meta.permissions().readonly())
}

#[cfg(target_os = "macos")]
fn read_only_volume(path: &Path) -> bool {
    let Some(path) = c_path(path) else {
        return false;
    };
    let mut stats: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(path.as_ptr(), &mut stats) } != 0 {
        return false;
    }
    stats.f_flags & libc::MNT_RDONLY as u32 != 0
}

#[cfg(not(target_os = "macos"))]
fn read_only_volume(_path: &Path) -> bool {
    false
}

/// Unpacks and checks a downloaded archive (see the module). Blocks.
pub fn prepare(archive: &Path, version: &crate::Version) -> Result<Prepared, String> {
    let requirement = match current_bundle() {
        Some(bundle) => Some(designated_requirement(&bundle)?),
        None => None,
    };
    prepare_with_requirement(archive, version, requirement.as_deref())
}

/// [`prepare`] with the designated requirement the new bundle must satisfy (`None` — any valid
/// signature: Flux isn't running from a bundle). [`Prepared::target`] is the running bundle, or
/// `/Applications/Flux.app` when there is none.
pub fn prepare_with_requirement(
    archive: &Path,
    version: &crate::Version,
    requirement: Option<&str>,
) -> Result<Prepared, String> {
    let dir = archive
        .parent()
        .ok_or_else(|| format!("{}: no folder", archive.display()))?
        .join("unpacked");
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    }
    std::fs::create_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    run(Command::new("/usr/bin/ditto")
        .arg("-x")
        .arg("-k")
        .arg(archive)
        .arg(&dir))
    .map_err(|err| format!("The update couldn't be unpacked: {err}"))?;

    let apps: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|err| format!("{}: {err}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "app") && path.is_dir())
        .collect();
    let [app] = apps.as_slice() else {
        return Err(format!(
            "The update holds {} applications instead of one Flux.app",
            apps.len()
        ));
    };

    let plist = app.join("Contents/Info.plist");
    let shown = run(Command::new("/usr/libexec/PlistBuddy")
        .arg("-c")
        .arg("Print :CFBundleShortVersionString")
        .arg(&plist))
    .map_err(|err| format!("The update's version can't be read: {err}"))?;
    let shown = shown.trim();
    if crate::Version::parse(shown).ok().as_ref() != Some(version) {
        return Err(format!(
            "The update is Flux {shown}, the release says {version}: it wasn't installed"
        ));
    }

    run(Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(app))
    .map_err(|err| format!("The update's code signature isn't valid: {err}"))?;
    if let Some(requirement) = requirement {
        run(Command::new("/usr/bin/codesign")
            .arg("--verify")
            .arg("--test-requirement")
            .arg(format!("={requirement}"))
            .arg(app))
        .map_err(|err| {
            format!(
                "The update isn't signed by the same certificate as this Flux: it wasn't \
                 installed ({err})"
            )
        })?;
    }

    Ok(Prepared {
        app: app.clone(),
        target: current_bundle().unwrap_or_else(|| PathBuf::from("/Applications/Flux.app")),
        version: version.clone(),
    })
}

/// The designated requirement of a signed bundle (`codesign -d -r-`): `identifier "…" and
/// certificate root = H"…"` for Flux's certificate, `cdhash H"…"` for an ad-hoc signature.
pub fn designated_requirement(bundle: &Path) -> Result<String, String> {
    let output = Command::new("/usr/bin/codesign")
        .args(["-d", "-r-"])
        .arg(bundle)
        .output()
        .map_err(|err| format!("codesign: {err}"))?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    text.lines()
        .find_map(|line| {
            // An implicit requirement (codesign made it, the signature has none) is printed as a
            // comment: `# designated => …`.
            let line = line.trim();
            let line = line.strip_prefix('#').map(str::trim_start).unwrap_or(line);
            line.strip_prefix("designated => ")
        })
        .map(|requirement| requirement.trim().to_string())
        .ok_or_else(|| {
            format!(
                "This Flux's code signature has no designated requirement ({}): {}",
                bundle.display(),
                text.trim()
            )
        })
}

/// Where the helper writes and what it does after the swap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Helper {
    /// The helper's log.
    pub log: PathBuf,
    /// Where the old bundle goes.
    pub trash: PathBuf,
    /// Open the new bundle after the swap.
    pub relaunch: bool,
    /// Environment the new Flux is opened with (`open --env`): an app opened by `open` doesn't
    /// inherit the helper's, so a Flux on a test profile (`scripts/update-test.sh`,
    /// `first-run.sh`: `FLUX_*_FILE`…) would come back on the user's real one.
    pub env: Vec<(String, String)>,
}

impl Helper {
    /// The user's: `~/Library/Logs/Flux/update.log` (`FLUX_LOG_DIR`, as Flux's log), `~/.Trash`; no relaunch with
    /// `FLUX_UPDATE_NO_RELAUNCH=1`; the new Flux gets this process's `FLUX_*` variables.
    pub fn user() -> Helper {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        Helper {
            log: std::env::var_os("FLUX_LOG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("Library/Logs/Flux"))
                .join("update.log"),
            trash: home.join(".Trash"),
            relaunch: std::env::var("FLUX_UPDATE_NO_RELAUNCH").as_deref() != Ok("1"),
            env: std::env::vars()
                .filter(|(name, _)| name.starts_with("FLUX_"))
                .collect(),
        }
    }
}

/// Starts the helper that swaps the bundles once the process `pid` exits, then opens the new one
/// with `relaunch` arguments. Call right before quitting.
pub fn schedule(prepared: &Prepared, pid: u32, relaunch: &[String]) -> Result<(), String> {
    schedule_with(prepared, pid, relaunch, &Helper::user())
}

/// [`schedule`] with another log, Trash and relaunch.
pub fn schedule_with(
    prepared: &Prepared,
    pid: u32,
    relaunch: &[String],
    helper: &Helper,
) -> Result<(), String> {
    let script = helper_script(prepared, pid, relaunch, helper);
    let dir = prepared
        .app
        .parent()
        .and_then(Path::parent)
        .unwrap_or(Path::new("/tmp"));
    let path = dir.join("install-update.sh");
    std::fs::write(&path, script).map_err(|err| format!("{}: {err}", path.display()))?;
    let mut command = Command::new("/usr/bin/nohup");
    command
        .arg("/bin/sh")
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
        .spawn()
        .map_err(|err| format!("The update's helper couldn't start: {err}"))?;
    Ok(())
}

/// The helper: wait for Flux to quit, old bundle to the Trash (or aside), new one in its place,
/// the old one back on any failure, then open Flux — whichever is in place.
fn helper_script(prepared: &Prepared, pid: u32, relaunch: &[String], helper: &Helper) -> String {
    let old_name = format!("Flux {}", crate::current_version());
    let trashed = unique(&helper.trash, &old_name);
    let target = &prepared.target;
    let aside = target.with_file_name(format!(
        ".{}.flux-old-{}",
        target
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Flux.app".into()),
        std::process::id()
    ));
    let args: Vec<String> = relaunch.iter().map(|arg| quote(arg)).collect();
    let env: String = helper
        .env
        .iter()
        .map(|(name, value)| format!("--env {} ", quote(&format!("{name}={value}"))))
        .collect();
    let open = if helper.relaunch {
        if args.is_empty() {
            format!("open {env}\"$TARGET\"")
        } else {
            format!("open {env}\"$TARGET\" --args {}", args.join(" "))
        }
    } else {
        "log \"not opening Flux (FLUX_UPDATE_NO_RELAUNCH)\"".to_string()
    };
    format!(
        r#"#!/bin/sh
# Flux's update helper (flux-update::install): swaps Flux.app once Flux has quit.
PID={pid}
NEW={new}
TARGET={target}
TRASH_DIR={trash_dir}
TRASHED={trashed}
ASIDE={aside}
LOG={log}
VERSION={version}
mkdir -p "$(dirname "$LOG")" 2>/dev/null
log() {{ printf '%s %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*" >> "$LOG"; }}
relaunch() {{
  {open}
}}
log "update to Flux $VERSION: waiting for Flux (pid $PID) to quit"
i=0
while kill -0 "$PID" 2>/dev/null; do
  i=$((i + 1))
  if [ "$i" -ge 1200 ]; then
    log "Flux didn't quit in 2 minutes: update cancelled"
    exit 1
  fi
  sleep 0.1
done
OLD=""
if [ -e "$TARGET" ]; then
  if mkdir -p "$TRASH_DIR" 2>/dev/null && mv "$TARGET" "$TRASHED" 2>>"$LOG"; then
    OLD="$TRASHED"
    log "the old Flux moved to $TRASHED"
  elif mv "$TARGET" "$ASIDE" 2>>"$LOG"; then
    OLD="$ASIDE"
    log "the old Flux moved aside to $ASIDE"
  else
    log "the old Flux can't be moved: update cancelled"
    relaunch
    exit 1
  fi
fi
if mv "$NEW" "$TARGET" 2>>"$LOG"; then
  log "Flux $VERSION installed at $TARGET"
  if [ "$OLD" = "$ASIDE" ]; then
    rm -rf "$ASIDE"
  fi
else
  log "the new Flux can't be put in place: restoring the old one"
  if [ -n "$OLD" ] && ! mv "$OLD" "$TARGET" 2>>"$LOG"; then
    log "the old Flux couldn't be restored: it is at $OLD"
  fi
fi
relaunch
log "done"
"#,
        new = quote_path(&prepared.app),
        target = quote_path(target),
        trash_dir = quote_path(&helper.trash),
        trashed = quote_path(&trashed),
        aside = quote_path(&aside),
        log = quote_path(&helper.log),
        version = quote(&prepared.version.to_string()),
    )
}

/// `dir/<name>.app`, or `dir/<name> 2.app`… if taken.
fn unique(dir: &Path, name: &str) -> PathBuf {
    let mut candidate = dir.join(format!("{name}.app"));
    let mut n = 2;
    while candidate.exists() {
        candidate = dir.join(format!("{name} {n}.app"));
        n += 1;
    }
    candidate
}

/// A word for `/bin/sh`: single quotes, a `'` inside as `'\''`.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn quote_path(path: &Path) -> String {
    quote(&path.to_string_lossy())
}

/// Runs a command; its stdout, or its stderr as the error.
fn run(command: &mut Command) -> Result<String, String> {
    let output = command.output().map_err(|err| err.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(if stderr.trim().is_empty() {
            output.status.to_string()
        } else {
            stderr.trim().to_string()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Version;
    use crate::download::tests::temp_dir;

    #[test]
    fn bundles() {
        assert_eq!(
            bundle_of(Path::new("/Applications/Flux.app/Contents/MacOS/flux")),
            Some(PathBuf::from("/Applications/Flux.app"))
        );
        assert_eq!(
            bundle_of(Path::new("/Users/me/flux/target/debug/flux")),
            None
        );
        assert_eq!(
            bundle_of(Path::new("/x/Flux.app/Contents/Resources/helper")),
            None
        );
        // The test binary itself isn't in a bundle.
        assert_eq!(can_install(), Err(CannotInstall::NotBundled));
    }

    #[test]
    fn installable() {
        let dir = temp_dir("installable");
        let app = dir.join("Flux.app");
        std::fs::create_dir_all(&app).unwrap();
        assert_eq!(check_installable(&app), Ok(()));
        assert_eq!(
            check_installable(Path::new(
                "/private/var/folders/x/AppTranslocation/ABC/d/Flux.app"
            )),
            Err(CannotInstall::Translocated)
        );
        assert!(matches!(
            check_installable(Path::new("/System/Applications/Flux.app")),
            Err(CannotInstall::ReadOnly(_) | CannotInstall::Translocated)
        ));
    }

    #[test]
    fn quoting() {
        assert_eq!(quote("plain"), "'plain'");
        assert_eq!(quote("it's"), "'it'\\''s'");
        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("printf %s {}", quote("a 'b' $HOME \"c\" `d`")))
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "a 'b' $HOME \"c\" `d`"
        );
    }

    /// A tiny `Flux.app` of `version`, ad-hoc signed.
    #[cfg(target_os = "macos")]
    fn fake_app(dir: &Path, version: &str, marker: &str) -> PathBuf {
        let app = dir.join("Flux.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>dev.flux.test</string>
<key>CFBundleExecutable</key><string>flux</string>
<key>CFBundleShortVersionString</key><string>{version}</string>
<key>CFBundleVersion</key><string>{version}</string>
<key>FluxMarker</key><string>{marker}</string>
</dict></plist>"#
            ),
        )
        .unwrap();
        let exe = app.join("Contents/MacOS/flux");
        std::fs::write(&exe, format!("#!/bin/sh\n# {marker}\nexit 0\n")).unwrap();
        run(Command::new("/bin/chmod").arg("755").arg(&exe)).unwrap();
        run(Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(&app))
        .unwrap();
        app
    }

    #[cfg(target_os = "macos")]
    fn zip(app: &Path, archive: &Path) {
        run(Command::new("/usr/bin/ditto")
            .args(["-c", "-k", "--keepParent"])
            .arg(app)
            .arg(archive))
        .unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn prepares_a_signed_bundle() {
        let root = temp_dir("prepare it's");
        let build = root.join("build");
        let app = fake_app(&build, "0.2.0", "new");
        let requirement = designated_requirement(&app).unwrap();
        assert!(requirement.contains("cdhash"), "{requirement}");
        let cache = root.join("cache 0.2.0");
        std::fs::create_dir_all(&cache).unwrap();
        let archive = cache.join("Flux-0.2.0-macos-aarch64.zip");
        zip(&app, &archive);
        let version = Version::parse("0.2.0").unwrap();

        let prepared = prepare_with_requirement(&archive, &version, Some(&requirement)).unwrap();
        assert_eq!(prepared.app, cache.join("unpacked/Flux.app"));
        assert_eq!(prepared.version, version);
        // Unpacking again starts from a fresh folder.
        prepare_with_requirement(&archive, &version, None).unwrap();

        let other = fake_app(&root.join("other"), "0.2.0", "other");
        let foreign = designated_requirement(&other).unwrap();
        let err = prepare_with_requirement(&archive, &version, Some(&foreign)).unwrap_err();
        assert!(err.contains("same certificate"), "{err}");

        let err = prepare_with_requirement(&archive, &Version::parse("0.3.0").unwrap(), None)
            .unwrap_err();
        assert!(err.contains("Flux 0.2.0"), "{err}");

        // A bundle changed after signing.
        std::fs::write(app.join("Contents/MacOS/flux"), "#!/bin/sh\nexit 1\n").unwrap();
        let tampered = cache.join("tampered.zip");
        zip(&app, &tampered);
        let err = prepare_with_requirement(&tampered, &version, None).unwrap_err();
        assert!(err.contains("code signature"), "{err}");

        let err = prepare_with_requirement(&root.join("missing.zip"), &version, None).unwrap_err();
        assert!(err.contains("unpacked"), "{err}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn swaps_after_quit() {
        let root = temp_dir("swap \"q\" it's");
        let installed = fake_app(&root.join("Applications"), "0.1.0", "old");
        let new = fake_app(&root.join("cache/unpacked"), "0.2.0", "new");
        let trash = root.join("Trash");
        let log = root.join("Logs/update.log");
        let mut child = Command::new("/bin/sleep").arg("0.3").spawn().unwrap();
        let prepared = Prepared {
            app: new.clone(),
            target: installed.clone(),
            version: Version::parse("0.2.0").unwrap(),
        };
        let helper = Helper {
            log: log.clone(),
            trash: trash.clone(),
            relaunch: false,
            env: Vec::new(),
        };
        schedule_with(
            &prepared,
            child.id(),
            &["--x".into(), "it's".into()],
            &helper,
        )
        .unwrap();
        child.wait().unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !std::fs::read_to_string(&log).is_ok_and(|text| text.contains("done")) {
            assert!(
                std::time::Instant::now() < deadline,
                "the helper didn't finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let plist = std::fs::read_to_string(installed.join("Contents/Info.plist")).unwrap();
        assert!(plist.contains("<string>new</string>"), "{plist}");
        assert!(!new.exists());
        let old = trash.join(format!("Flux {}.app", crate::current_version()));
        let old_plist = std::fs::read_to_string(old.join("Contents/Info.plist")).unwrap();
        assert!(old_plist.contains("<string>old</string>"));
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("Flux 0.2.0 installed"), "{text}");
        assert!(text.contains("not opening Flux"), "{text}");
    }

    #[test]
    fn script_relaunches_with_arguments() {
        let prepared = Prepared {
            app: PathBuf::from("/c/unpacked/Flux.app"),
            target: PathBuf::from("/Applications/Flux.app"),
            version: Version::parse("0.2.0").unwrap(),
        };
        let helper = Helper {
            log: PathBuf::from("/l/update.log"),
            trash: PathBuf::from("/t"),
            relaunch: true,
            env: Vec::new(),
        };
        let script = helper_script(&prepared, 42, &["/my project".into()], &helper);
        assert!(
            script.contains("open \"$TARGET\" --args '/my project'"),
            "{script}"
        );
        assert!(script.contains("PID=42"));
        let bare = helper_script(&prepared, 42, &[], &helper);
        assert!(bare.contains("open \"$TARGET\"\n"), "{bare}");
        let profile = Helper {
            env: vec![("FLUX_SETTINGS_FILE".into(), "/tmp/a b/settings.json".into())],
            ..helper.clone()
        };
        let with_env = helper_script(&prepared, 42, &[], &profile);
        assert!(
            with_env.contains("open --env 'FLUX_SETTINGS_FILE=/tmp/a b/settings.json' \"$TARGET\""),
            "{with_env}"
        );
        let status = Command::new("/bin/sh")
            .arg("-n")
            .arg("-c")
            .arg(&script)
            .status()
            .unwrap();
        assert!(status.success());
    }
}
