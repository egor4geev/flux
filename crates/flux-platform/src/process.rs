//! Starting and finding other programs: git, language servers, the Claude CLI, the plugins'
//! programs, `!` commands.

use std::ffi::OsStr;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A program Flux starts for its own work, out of sight.
pub trait Background {
    /// On Windows a console program started by a windowed app opens a console window of its own:
    /// every git call would flash one. `CREATE_NO_WINDOW` gives it a console nobody sees. Elsewhere
    /// nothing changes.
    fn background(&mut self) -> &mut Self;
}

impl Background for Command {
    fn background(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            self.creation_flags(CREATE_NO_WINDOW);
        }
        self
    }
}

/// A file the system runs as a program: one with an executable bit (a link to one counts:
/// Homebrew's programs are links); on Windows — one with an extension `PATHEXT` lists.
pub fn is_executable(file: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(file)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(windows)]
    {
        let extension = file.extension().map(|ext| ext.to_string_lossy());
        file.is_file()
            && extension.is_some_and(|ext| {
                path_extensions().any(|known| known[1..].eq_ignore_ascii_case(&ext))
            })
    }
}

/// The files a program name may be in one folder: the name itself, and on Windows a bare name with
/// each extension of `PATHEXT` (`cargo` → `cargo.exe`, `npx` → `npx.cmd`).
pub fn program_files(dir: &Path, program: &str) -> Vec<PathBuf> {
    let mut files = vec![dir.join(program)];
    if cfg!(windows) && Path::new(program).extension().is_none() {
        files.extend(path_extensions().map(|ext| dir.join(format!("{program}{ext}"))));
    }
    files
}

/// The program a name stands for in the folders of `path` (a `PATH` value), first found first.
pub fn find_in_path(program: &str, path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .flat_map(|dir| program_files(&dir, program))
        .find(|file| is_executable(file))
}

/// Windows' program extensions, `.exe` first: `PATHEXT`, lowercased.
fn path_extensions() -> impl Iterator<Item = String> {
    let extensions = std::env::var("PATHEXT").unwrap_or_default();
    let extensions = if extensions.is_empty() {
        ".COM;.EXE;.BAT;.CMD".to_string()
    } else {
        extensions
    };
    extensions
        .split(';')
        .filter(|ext| ext.len() > 1 && ext.starts_with('.'))
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .into_iter()
}

/// The process is there (someone else's counts too).
pub fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: signal 0 only checks that the process exists.
        if unsafe { libc::kill(pid, 0) } == 0 {
            return true;
        }
        // EPERM: it does, someone else's.
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        /// `GetExitCodeProcess` of a process still running.
        const STILL_ACTIVE: u32 = 259;
        // SAFETY: the handle is checked before use and closed after.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                // Denied: it does, someone else's.
                return GetLastError() == ERROR_ACCESS_DENIED;
            }
            let mut code = 0;
            let running = GetExitCodeProcess(process, &mut code) != 0 && code == STILL_ACTIVE;
            CloseHandle(process);
            running
        }
    }
}

/// Nobody reads this process's stderr: it is closed or `/dev/null` (Finder, the Dock and `open`
/// start apps so); on Windows a windowed app has none at all.
pub fn stderr_unread() -> bool {
    #[cfg(unix)]
    // SAFETY: fstat and stat write only into the zeroed structs, which outlive the calls.
    unsafe {
        let mut stderr: libc::stat = std::mem::zeroed();
        if libc::fstat(libc::STDERR_FILENO, &mut stderr) != 0 {
            return true;
        }
        let mut null: libc::stat = std::mem::zeroed();
        if libc::stat(c"/dev/null".as_ptr(), &mut null) != 0 {
            return false;
        }
        (stderr.st_mode & libc::S_IFMT) == libc::S_IFCHR && stderr.st_rdev == null.st_rdev
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
        // SAFETY: only reads the process's standard handle.
        let handle = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
        handle.is_null() || handle == INVALID_HANDLE_VALUE
    }
}

/// Sends this process's stderr (what Rust's `eprintln!` and panics write) into `file`.
pub fn redirect_stderr(file: &File) -> bool {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: both descriptors are open; dup2 replaces fd 2 atomically.
        unsafe { libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO) >= 0 }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::Console::{STD_ERROR_HANDLE, SetStdHandle};
        // SAFETY: the handle stays open while the log keeps the file; Rust's stderr asks for the
        // standard handle at every write.
        unsafe { SetStdHandle(STD_ERROR_HANDLE, file.as_raw_handle()) != 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_alive() {
        assert!(alive(std::process::id()));
        assert!(!alive(u32::MAX - 1));
    }

    #[test]
    fn programs_are_found_in_the_path() {
        let dir = std::env::temp_dir().join(format!("flux-platform-path-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        let program = {
            use std::os::unix::fs::PermissionsExt;
            let program = dir.join("tool");
            std::fs::write(&program, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            program
        };
        #[cfg(windows)]
        let program = {
            let program = dir.join("tool.cmd");
            std::fs::write(&program, "@echo off\r\n").unwrap();
            program
        };
        std::fs::write(dir.join("text"), "").unwrap();
        let path = std::env::join_paths([Path::new("/nowhere"), &dir]).unwrap();
        assert_eq!(find_in_path("tool", &path), Some(program));
        assert_eq!(find_in_path("text", &path), None);
        assert_eq!(find_in_path("missing", &path), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
