//! What runs in a terminal: the foreground process of the PTY, its name and working directory.
//!
//! macOS asks the kernel through libproc; Linux reads `/proc`; Windows looks through a snapshot of
//! the processes. Everything here is a few system calls, cheap enough to call after each burst of
//! output.

#[cfg(unix)]
use std::os::fd::RawFd;
use std::path::PathBuf;

/// The process group in the foreground of the PTY (its leader's pid): the shell while it waits for a
/// command, the command while it runs. An interactive shell puts every command into a group of its
/// own (job control); a non-interactive `sh -c` doesn't, so its children never show up here.
#[cfg(unix)]
pub(crate) fn foreground_pid(pty: RawFd) -> Option<u32> {
    // SAFETY: `tcgetpgrp` only reads the terminal's state; an invalid descriptor makes it fail.
    let pgid = unsafe { libc::tcgetpgrp(pty) };
    u32::try_from(pgid).ok().filter(|&pgid| pgid > 0)
}

/// A Windows console has no foreground process group: what runs is found among the processes the
/// shell started ([`command_in`]); the shell itself while it waits for a command.
#[cfg(windows)]
pub(crate) fn foreground_pid(shell: u32) -> Option<u32> {
    use flux_platform::process;
    (shell > 0).then(|| command_in(&process::list(), shell, process::started))
}

/// The command running in `shell`, as a Unix terminal's foreground group would show it: the newest
/// program the shell started; when that is a shell itself (`cmd /c` running an `npm.cmd`, Git
/// Bash's launcher), the newest program it started, and so on. Console hosts are no commands. Only
/// shells found — nothing runs: `shell` itself. A child counts only when it started after its
/// parent (`started`): Windows gives an ended process's id to new ones.
#[cfg(any(windows, test))]
fn command_in(
    list: &[flux_platform::process::ProcessEntry],
    shell: u32,
    started: impl Fn(u32) -> Option<u64>,
) -> u32 {
    let Some(mut parent_started) = started(shell) else {
        return shell;
    };
    let mut parent = shell;
    // A chain of shells deeper than this is a loop of reused ids.
    for _ in 0..16 {
        let newest = list
            .iter()
            .filter(|entry| entry.parent == parent && entry.pid != parent && entry.pid != shell)
            .filter(|entry| !is_console_host(&entry.name))
            .filter_map(|entry| Some((entry, started(entry.pid)?)))
            .filter(|&(_, at)| at >= parent_started)
            .max_by_key(|&(_, at)| at);
        match newest {
            Some((entry, at)) if is_shell(&entry.name) => {
                parent = entry.pid;
                parent_started = at;
            }
            Some((entry, _)) => return entry.pid,
            None => break,
        }
    }
    shell
}

/// A shell or a command interpreter: what it runs is the command, not it.
#[cfg(any(windows, test))]
fn is_shell(name: &str) -> bool {
    const SHELLS: [&str; 9] = [
        "cmd",
        "powershell",
        "pwsh",
        "bash",
        "sh",
        "zsh",
        "fish",
        "nu",
        "wsl",
    ];
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    SHELLS.iter().any(|shell| shell.eq_ignore_ascii_case(stem))
}

/// The console host of a pseudoconsole: part of the terminal, not something the user started.
#[cfg(any(windows, test))]
fn is_console_host(name: &str) -> bool {
    name.eq_ignore_ascii_case("conhost.exe") || name.eq_ignore_ascii_case("OpenConsole.exe")
}

/// The process name ("zsh", "cargo").
#[cfg(target_os = "macos")]
pub(crate) fn name(pid: u32) -> Option<String> {
    let pid = libc::c_int::try_from(pid).ok()?;
    // `proc_name` gives the short name the process set or its executable's name (up to 2 × MAXCOMLEN).
    let mut buffer = [0u8; 2 * 16 + 1];
    // SAFETY: the buffer outlives the call and its size is passed along.
    let len = unsafe { libc::proc_name(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if let Some(name) = utf8(&buffer, len).filter(|name| !name.is_empty()) {
        return Some(name);
    }
    // No name: the file name of the executable.
    let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: as above.
    let len = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
    let path = utf8(&path, len)?;
    let name = path.rsplit('/').next()?;
    (!name.is_empty()).then(|| name.to_string())
}

/// The process's current directory.
#[cfg(target_os = "macos")]
pub(crate) fn cwd(pid: u32) -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::mem::{MaybeUninit, size_of};

    let pid = libc::c_int::try_from(pid).ok()?;
    let mut info = MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let size = size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    // SAFETY: the kernel fills at most `size` bytes of `info`, which lives through the call.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    // SAFETY: the whole struct was written (and it started zeroed anyway).
    let info = unsafe { info.assume_init() };
    // `vip_path` is a NUL-terminated `char[MAXPATHLEN]`, split into rows by libc's definition.
    let path = info.pvi_cdir.vip_path.as_flattened();
    // SAFETY: `c_char` and `u8` have the same size and layout.
    let bytes = unsafe { std::slice::from_raw_parts(path.as_ptr().cast::<u8>(), path.len()) };
    let path = CStr::from_bytes_until_nul(bytes).ok()?.to_str().ok()?;
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// The UTF-8 text a libproc call wrote: `len` bytes, or up to the first NUL.
#[cfg(target_os = "macos")]
fn utf8(buffer: &[u8], len: libc::c_int) -> Option<String> {
    let len = usize::try_from(len).ok().filter(|&len| len > 0)?;
    let bytes = &buffer[..len.min(buffer.len())];
    let bytes = bytes.split(|&b| b == 0).next().unwrap_or(bytes);
    std::str::from_utf8(bytes).ok().map(str::to_string)
}

/// The process name ("zsh", "cargo").
#[cfg(target_os = "linux")]
pub(crate) fn name(pid: u32) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let name = comm.trim_end_matches('\n');
    (!name.is_empty()).then(|| name.to_string())
}

/// The process's current directory.
#[cfg(target_os = "linux")]
pub(crate) fn cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// The process name: its executable's file name without the extension ("pwsh", "cargo").
#[cfg(windows)]
pub(crate) fn name(pid: u32) -> Option<String> {
    let path = flux_platform::process::executable(pid)?;
    let name = path.file_stem()?.to_string_lossy().into_owned();
    (!name.is_empty()).then_some(name)
}

/// Another process's current directory isn't something Windows tells (it sits in the process's
/// memory): the terminal keeps the directory it started in.
#[cfg(windows)]
pub(crate) fn cwd(_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub(crate) fn name(_pid: u32) -> Option<String> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub(crate) fn cwd(_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The command a Windows shell runs: through wrapper shells to the program; the shell itself
    /// when only shells (a launcher, an idle nested shell) are there, or a reused id.
    #[test]
    fn the_command_is_found_among_the_shell_s_children() {
        use flux_platform::process::ProcessEntry;
        let entry = |pid: u32, parent: u32, name: &str| ProcessEntry {
            pid,
            parent,
            name: name.to_string(),
        };
        let started = |pid: u32| Some(u64::from(pid));
        // Idle.
        let list = [entry(10, 1, "pwsh.exe"), entry(11, 10, "conhost.exe")];
        assert_eq!(command_in(&list, 10, started), 10);
        // `npm run dev`: pwsh → cmd /c npm.cmd → node; the newest child wins.
        let list = [
            entry(10, 1, "pwsh.exe"),
            entry(20, 10, "git.exe"),
            entry(30, 10, "cmd.exe"),
            entry(40, 30, "node.exe"),
        ];
        assert_eq!(command_in(&list, 10, started), 40);
        // `cargo build`: cargo, not the compilers it runs.
        let list = [
            entry(10, 1, "pwsh.exe"),
            entry(20, 10, "cargo.exe"),
            entry(30, 20, "rustc.exe"),
        ];
        assert_eq!(command_in(&list, 10, started), 20);
        // Git Bash: the launcher's bash, waiting for a command.
        let list = [entry(10, 1, "bash.exe"), entry(20, 10, "bash.exe")];
        assert_eq!(command_in(&list, 10, started), 10);
        // A process that started before the shell names its id: someone else's.
        let list = [entry(10, 1, "pwsh.exe"), entry(5, 10, "vim.exe")];
        assert_eq!(command_in(&list, 10, started), 10);
        assert!(is_shell("PowerShell.exe") && is_shell("wsl") && !is_shell("node.exe"));
    }

    #[test]
    fn this_process_has_a_name_and_a_directory() {
        let pid = std::process::id();
        let name = name(pid).expect("name of the test process");
        assert!(!name.is_empty());
        #[cfg(unix)]
        {
            let cwd = cwd(pid).expect("directory of the test process");
            let expected = std::env::current_dir().unwrap();
            assert_eq!(
                std::fs::canonicalize(cwd).unwrap(),
                std::fs::canonicalize(expected).unwrap()
            );
        }
    }

    #[test]
    fn a_missing_process_has_nothing() {
        // Pids are below 100 000 on macOS and below 2^22 on Linux.
        assert_eq!(name(u32::MAX / 2), None);
        assert_eq!(cwd(u32::MAX / 2), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_descriptor_that_is_not_a_terminal_has_no_foreground() {
        assert_eq!(foreground_pid(-1), None);
        let file = std::fs::File::open("/dev/null").unwrap();
        assert_eq!(foreground_pid(std::os::fd::AsRawFd::as_raw_fd(&file)), None);
    }
}
