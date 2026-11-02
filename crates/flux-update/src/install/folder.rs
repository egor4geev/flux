//! Installing an update on Windows (part 10.9), where Flux is a folder: `Flux.exe` and its files,
//! in `%LOCALAPPDATA%\Programs\Flux` (the installer) or wherever its zip was unpacked.
//!
//! 1. [`prepare`]: the archive (a zip with the files at its root) is unpacked next to it by
//!    Windows' own `tar.exe` and must hold `Flux.exe`. Its signature was checked by the download.
//! 2. [`install`], as Flux quits: the new files are copied into a hidden folder inside Flux's
//!    (`.flux-new-<pid>`, so that the rest are renames on one volume); then every file in the way is
//!    renamed aside into `.flux-old-<pid>` — Windows lets the files of a running program be renamed,
//!    not overwritten or deleted — and the new one renamed into its place. A failure renames
//!    everything back. Then the new `Flux.exe` starts with [`WAIT_PID`] (the old one, if the swap
//!    failed): the projects reopen as on any start.
//! 3. [`finish`], first thing in the new Flux: it waits for the previous process to exit (its
//!    session saved, its log closed), then deletes the `.flux-old-*` and `.flux-new-*` folders.
//!
//! No helper process, unlike macOS: the swap needs nothing but renames. Every step is logged to
//! `update.log` in Flux's logs folder.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use std::{fs, io};

use super::{Helper, Prepared};

/// The program in Flux's folder, as the zip and the installer name it — cargo's own build is
/// `flux.exe`, which isn't updated over the air.
pub const EXE: &str = "Flux.exe";

/// The environment variable of a Flux started by its update: the process id of the Flux it
/// replaces, to wait for.
pub const WAIT_PID: &str = "FLUX_UPDATE_WAIT_PID";

/// Prefixes of the folders an update makes inside Flux's: the new files on their way in, the old
/// ones moved aside (the installer's uninstaller deletes them too).
const NEW_PREFIX: &str = ".flux-new-";
const OLD_PREFIX: &str = ".flux-old-";

/// How long the new Flux waits for the old one to exit.
const WAIT: Duration = Duration::from_secs(120);

/// The folder of Flux's files when `exe` is `Flux.exe`.
pub fn folder_of(exe: &Path) -> Option<PathBuf> {
    (exe.file_name()? == EXE)
        .then(|| exe.parent().map(Path::to_path_buf))
        .flatten()
}

/// Unpacks the archive next to it and checks it holds `Flux.exe`. Blocks.
pub fn prepare(archive: &Path, version: &crate::Version) -> Result<Prepared, String> {
    let dir = archive
        .parent()
        .ok_or_else(|| format!("{}: no folder", archive.display()))?
        .join("unpacked");
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    }
    fs::create_dir_all(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    super::run(
        flux_platform::process::Background::background(&mut Command::new(tar()))
            .arg("-xf")
            .arg(archive)
            .arg("-C")
            .arg(&dir),
    )
    .map_err(|err| format!("The update couldn't be unpacked: {err}"))?;
    if !dir.join(EXE).is_file() {
        return Err(format!("The update holds no {EXE}"));
    }
    let target = super::current_bundle().unwrap_or_else(|| {
        flux_platform::paths::home()
            .unwrap_or_default()
            .join("AppData")
            .join("Local")
            .join("Programs")
            .join("Flux")
    });
    Ok(Prepared {
        app: dir,
        target,
        version: version.clone(),
    })
}

/// Windows' `tar.exe` (bsdtar, since Windows 10 1803): it unpacks zips.
fn tar() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32").join("tar.exe"))
        .unwrap_or_else(|| PathBuf::from("tar.exe"))
}

/// Swaps the files (see the module) and starts Flux again, as the process `pid` quits.
pub fn install(
    prepared: &Prepared,
    pid: u32,
    relaunch: &[String],
    helper: &Helper,
) -> Result<(), String> {
    let target = &prepared.target;
    log(
        helper,
        &format!(
            "update to Flux {}: installing into {}",
            prepared.version,
            target.display()
        ),
    );
    let swapped = swap(&prepared.app, target, &pid.to_string());
    match &swapped {
        Ok(()) => log(helper, &format!("Flux {} installed", prepared.version)),
        Err(err) => log(
            helper,
            &format!("the update failed, the old Flux stays: {err}"),
        ),
    }
    if helper.relaunch {
        let exe = target.join(EXE);
        let started = Command::new(&exe)
            .args(relaunch)
            .envs(helper.env.iter().map(|(name, value)| (name, value)))
            .env(WAIT_PID, pid.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match started {
            Ok(_) => log(helper, &format!("started {}", exe.display())),
            Err(err) => log(helper, &format!("{} can't start: {err}", exe.display())),
        }
    } else {
        log(helper, "not starting Flux (FLUX_UPDATE_NO_RELAUNCH)");
    }
    swapped
}

/// Puts the files of `new` into `target`: copied into `target/.flux-new-<tag>`, then each renamed
/// into place, the file it replaces renamed into `target/.flux-old-<tag>`. On a failure every
/// rename is undone and `target` is as it was. Files of `target` the update doesn't have stay.
pub(crate) fn swap(new: &Path, target: &Path, tag: &str) -> Result<(), String> {
    let staging = target.join(format!("{NEW_PREFIX}{tag}"));
    let aside = target.join(format!("{OLD_PREFIX}{tag}"));
    let _ = fs::remove_dir_all(&staging);
    if let Err(err) = copy_dir(new, &staging) {
        let _ = fs::remove_dir_all(&staging);
        return Err(format!("{}: {err}", staging.display()));
    }
    let mut done = Vec::new();
    let result = fs::create_dir_all(&aside).and_then(|()| {
        for entry in fs::read_dir(&staging)? {
            let name = entry?.file_name();
            let place = target.join(&name);
            if place.symlink_metadata().is_ok() {
                rename(&place, &aside.join(&name), &mut done)?;
            }
            rename(&staging.join(&name), &place, &mut done)?;
        }
        Ok(())
    });
    if let Err(err) = result {
        for (from, to) in done.into_iter().rev() {
            let _ = fs::rename(&to, &from);
        }
        let _ = fs::remove_dir_all(&staging);
        let _ = fs::remove_dir(&aside);
        return Err(err.to_string());
    }
    let _ = fs::remove_dir(&staging);
    Ok(())
}

/// A rename, remembered to be undone.
fn rename(from: &Path, to: &Path, done: &mut Vec<(PathBuf, PathBuf)>) -> io::Result<()> {
    fs::rename(from, to)
        .map_err(|err| io::Error::new(err.kind(), format!("{}: {err}", from.display())))?;
    done.push((from.to_path_buf(), to.to_path_buf()));
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            copy_dir(&path, &to.join(entry.file_name()))?;
        } else {
            fs::copy(&path, to.join(entry.file_name()))?;
        }
    }
    Ok(())
}

/// Deletes what updates left in `folder`: the files of the versions before, a staging folder of an
/// update that didn't finish. Returns what couldn't be deleted (a file of a program still running),
/// for the next start.
pub(crate) fn remove_leftovers(folder: &Path) -> Vec<String> {
    let mut failed = Vec::new();
    for entry in fs::read_dir(folder).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if (name.starts_with(NEW_PREFIX) || name.starts_with(OLD_PREFIX))
            && let Err(err) = fs::remove_dir_all(entry.path())
        {
            failed.push(format!("{}: {err}", entry.path().display()));
        }
    }
    failed
}

/// The end of an update, first thing in `main` (see the module): with [`WAIT_PID`] — this Flux
/// was started by its update — waits up to two minutes for the previous one to exit; then deletes
/// the previous version's files. Does nothing outside Windows.
///
/// # Safety
///
/// Removes [`WAIT_PID`] from the environment (Flux's terminals and programs mustn't inherit it):
/// call it before any other thread runs.
pub unsafe fn finish() {
    if !cfg!(windows) {
        return;
    }
    if let Some(pid) = std::env::var_os(WAIT_PID) {
        // SAFETY: the caller's promise — no other thread yet.
        unsafe { std::env::remove_var(WAIT_PID) };
        if let Some(pid) = pid.to_str().and_then(|pid| pid.parse::<u32>().ok()) {
            let start = Instant::now();
            while flux_platform::process::alive(pid) && start.elapsed() < WAIT {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    let Some(folder) = super::current_bundle() else {
        return;
    };
    let helper = Helper::user();
    for failed in remove_leftovers(&folder) {
        log(&helper, &format!("left for the next start: {failed}"));
    }
}

/// A line in the update's log, as the macOS helper writes it.
fn log(helper: &Helper, line: &str) {
    use std::io::Write as _;
    if let Some(dir) = helper.log.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&helper.log)
    else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64);
    let local = now + i64::from(flux_platform::time::local_utc_offset(now));
    let (year, month, day, hour, minute, second) = flux_platform::time::civil(local);
    let _ = writeln!(
        file,
        "{year}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} {line}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::tests::temp_dir;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn folders() {
        assert_eq!(
            folder_of(Path::new("/Users/me/AppData/Local/Programs/Flux/Flux.exe")),
            Some(PathBuf::from("/Users/me/AppData/Local/Programs/Flux"))
        );
        // cargo's build.
        assert_eq!(folder_of(Path::new("/x/target/release/flux.exe")), None);
        assert_eq!(folder_of(Path::new("/x/Flux/other.exe")), None);
    }

    #[test]
    fn swaps_the_files() {
        let dir = temp_dir("swap");
        let new = dir.join("unpacked");
        let target = dir.join("Flux");
        write(&new.join(EXE), "new exe");
        write(&new.join("LICENSE.txt"), "new licence");
        write(&new.join("resources/a.txt"), "new resource");
        write(&target.join(EXE), "old exe");
        write(&target.join("LICENSE.txt"), "old licence");
        write(&target.join("resources/b.txt"), "old resource");
        write(&target.join("notes.txt"), "the user's");

        swap(&new, &target, "7").unwrap();
        assert_eq!(read(&target.join(EXE)), "new exe");
        assert_eq!(read(&target.join("LICENSE.txt")), "new licence");
        // A folder is replaced whole, the user's file stays.
        assert_eq!(read(&target.join("resources/a.txt")), "new resource");
        assert!(!target.join("resources/b.txt").exists());
        assert_eq!(read(&target.join("notes.txt")), "the user's");
        // The old files aside, the staging folder gone; the unpacked update is left as it was.
        assert_eq!(read(&target.join(".flux-old-7").join(EXE)), "old exe");
        assert!(!target.join(".flux-new-7").exists());
        assert_eq!(read(&new.join(EXE)), "new exe");

        assert!(remove_leftovers(&target).is_empty());
        assert!(!target.join(".flux-old-7").exists());
        assert_eq!(read(&target.join(EXE)), "new exe");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_swap_changes_nothing() {
        let dir = temp_dir("swap-failed");
        let new = dir.join("unpacked");
        let target = dir.join("Flux");
        write(&new.join(EXE), "new exe");
        write(&target.join(EXE), "old exe");
        // The folder for the old files can't be made: a file is in its place.
        write(&target.join(".flux-old-8"), "in the way");

        assert!(swap(&new, &target, "8").is_err());
        assert_eq!(read(&target.join(EXE)), "old exe");
        assert!(!target.join(".flux-new-8").exists());
        // An update with nothing to unpack from fails before touching the folder.
        assert!(swap(&dir.join("missing"), &target, "9").is_err());
        assert_eq!(read(&target.join(EXE)), "old exe");
        assert!(!target.join(".flux-new-9").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
