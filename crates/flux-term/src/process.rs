//! What runs in a terminal: the foreground process of the PTY, its name and working directory.
//!
//! macOS asks the kernel through libproc; Linux reads `/proc`. Everything here is a few syscalls,
//! cheap enough to call after each burst of output.

use std::os::fd::RawFd;
use std::path::PathBuf;

/// The process group in the foreground of the PTY (its leader's pid): the shell while it waits for a
/// command, the command while it runs. An interactive shell puts every command into a group of its
/// own (job control); a non-interactive `sh -c` doesn't, so its children never show up here.
pub(crate) fn foreground_pid(pty: RawFd) -> Option<u32> {
    // SAFETY: `tcgetpgrp` only reads the terminal's state; an invalid descriptor makes it fail.
    let pgid = unsafe { libc::tcgetpgrp(pty) };
    u32::try_from(pgid).ok().filter(|&pgid| pgid > 0)
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

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn name(_pid: u32) -> Option<String> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn cwd(_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_has_a_name_and_a_directory() {
        let pid = std::process::id();
        let name = name(pid).expect("name of the test process");
        assert!(!name.is_empty());
        let cwd = cwd(pid).expect("directory of the test process");
        let expected = std::env::current_dir().unwrap();
        assert_eq!(
            std::fs::canonicalize(cwd).unwrap(),
            std::fs::canonicalize(expected).unwrap()
        );
    }

    #[test]
    fn a_missing_process_has_nothing() {
        // Pids are below 100 000 on macOS and below 2^22 on Linux.
        assert_eq!(name(u32::MAX / 2), None);
        assert_eq!(cwd(u32::MAX / 2), None);
    }

    #[test]
    fn a_descriptor_that_is_not_a_terminal_has_no_foreground() {
        assert_eq!(foreground_pid(-1), None);
        let file = std::fs::File::open("/dev/null").unwrap();
        assert_eq!(foreground_pid(std::os::fd::AsRawFd::as_raw_fd(&file)), None);
    }
}
