//! Processes started from Flux that outlive it.
//!
//! macOS counts every process Flux starts, and everything those start in turn, as part of Flux (its
//! coalition): a daemon a command in a Flux terminal left behind — a container VM manager started by
//! `docker`, a build daemon, `nohup … &` — keeps Flux "open" in the Dock after Flux itself has quit,
//! until that process ends (LaunchServices reports the app as "exited with subordinates"). Before
//! quitting Flux names such processes and offers to terminate them.
//!
//! Only processes that have already left their parent (adopted by launchd) are counted: Flux's own
//! helpers (language servers, Claude, plugins) and the commands of open terminals end with Flux.

/// A process of Flux's coalition running on its own.
#[derive(Debug, Clone, PartialEq)]
pub struct Leftover {
    pub pid: i32,
    pub name: String,
    /// When it started (seconds, microseconds): the pid is checked again before a signal, it may have
    /// been reused.
    started: (u64, u64),
}

/// The processes of Flux's coalition whose parent is launchd, Flux itself excepted.
#[cfg(target_os = "macos")]
pub fn detached() -> Vec<Leftover> {
    let me = std::process::id() as i32;
    let Some(coalition) = coalition_of(me) else {
        return Vec::new();
    };
    all_pids()
        .into_iter()
        .filter(|&pid| pid != me && pid > 1)
        .filter(|&pid| coalition_of(pid) == Some(coalition))
        .filter_map(info)
        .filter(|(_, parent)| *parent == 1)
        .map(|(leftover, _)| leftover)
        .collect()
}

#[cfg(not(target_os = "macos"))]
pub fn detached() -> Vec<Leftover> {
    Vec::new()
}

/// Asks the processes to end (SIGTERM), skipping a pid that now belongs to another process.
#[cfg(target_os = "macos")]
pub fn terminate(leftovers: &[Leftover]) {
    for leftover in leftovers {
        if info(leftover.pid).is_none_or(|(now, _)| now.started != leftover.started) {
            continue;
        }
        // SAFETY: `kill` only sends a signal; a pid that is gone makes it fail.
        unsafe {
            libc::kill(leftover.pid, libc::SIGTERM);
        }
    }
}

/// Elsewhere none are found ([`detached`]).
#[cfg(not(target_os = "macos"))]
pub fn terminate(_leftovers: &[Leftover]) {}

/// The process names, for a dialog.
pub fn names(leftovers: &[Leftover]) -> Vec<String> {
    leftovers
        .iter()
        .map(|leftover| leftover.name.clone())
        .collect()
}

/// `proc_pidinfo` flavor `PROC_PIDCOALITIONINFO` (`sys/proc_info.h` of xnu, not in the SDK's
/// headers): the ids of the coalitions the process belongs to, by coalition type.
#[cfg(target_os = "macos")]
const PROC_PIDCOALITIONINFO: libc::c_int = 20;

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Default)]
struct CoalitionInfo {
    /// `COALITION_TYPE_RESOURCE`, `COALITION_TYPE_JETSAM`.
    ids: [u64; 2],
    reserved: [u64; 3],
}

/// The jetsam coalition: the one LaunchServices tracks an app by (`lsappinfo` prints it).
#[cfg(target_os = "macos")]
fn coalition_of(pid: libc::c_int) -> Option<u64> {
    let mut info = CoalitionInfo::default();
    let size = size_of::<CoalitionInfo>() as libc::c_int;
    // SAFETY: the kernel writes at most `size` bytes into `info`, which lives through the call.
    let written =
        unsafe { libc::proc_pidinfo(pid, PROC_PIDCOALITIONINFO, 0, (&raw mut info).cast(), size) };
    (written == size && info.ids[1] != 0).then_some(info.ids[1])
}

/// Every pid of the system.
#[cfg(target_os = "macos")]
fn all_pids() -> Vec<libc::c_int> {
    // SAFETY: a null buffer asks for the count only.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let Ok(count) = usize::try_from(count) else {
        return Vec::new();
    };
    // Room for processes started in the meantime.
    let mut pids = vec![0 as libc::c_int; count + 64];
    let size = (pids.len() * size_of::<libc::c_int>()) as libc::c_int;
    // SAFETY: the buffer outlives the call and its size in bytes is passed along.
    let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), size) };
    pids.truncate(usize::try_from(count).unwrap_or(0));
    pids
}

/// The process as a [`Leftover`], with its parent's pid.
#[cfg(target_os = "macos")]
fn info(pid: libc::c_int) -> Option<(Leftover, u32)> {
    use std::ffi::CStr;
    use std::mem::MaybeUninit;

    let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: the kernel fills at most `size` bytes of `info`, which lives through the call.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    // SAFETY: the whole struct was written.
    let info = unsafe { info.assume_init() };
    let text = |chars: &[libc::c_char]| {
        // SAFETY: `c_char` and `u8` have the same size and layout.
        let bytes = unsafe { std::slice::from_raw_parts(chars.as_ptr().cast::<u8>(), chars.len()) };
        CStr::from_bytes_until_nul(bytes)
            .ok()
            .and_then(|name| name.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    let name = Some(text(&info.pbi_name))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| text(&info.pbi_comm));
    Some((
        Leftover {
            pid,
            name,
            started: (info.pbi_start_tvsec, info.pbi_start_tvusec),
        },
        info.pbi_ppid,
    ))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn a_process_belongs_to_a_coalition() {
        let me = std::process::id() as i32;
        assert!(coalition_of(me).is_some());
        let (me, parent) = info(me).unwrap();
        assert!(!me.name.is_empty());
        assert!(parent > 0);
    }

    #[test]
    fn the_test_runner_leaves_nothing_detached_behind_it() {
        // The test process itself is never counted, whatever the coalition holds.
        let me = std::process::id() as i32;
        assert!(detached().iter().all(|leftover| leftover.pid != me));
    }
}
