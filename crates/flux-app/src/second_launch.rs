//! A second launch of Flux on Windows (part 10.9): `Flux.exe <folder or files>` while Flux runs —
//! "Open with Flux" in Explorer, the taskbar's jump list, a terminal — hands its folders and files
//! to the running Flux and ends, as macOS sends them to the running app (`open_urls.rs`). Without
//! any, the running Flux's window comes forward.
//!
//! The running Flux listens on a named pipe of the user's ([`pipe_name`]; the pipes' namespace is
//! the machine's): a launch that can write to it sends the paths, one per line ([`encode`]), and
//! ends; one that can't is the running Flux from then on. Other users can't write to the pipe (its
//! default security lets only its creator, the system and administrators write), nor other
//! machines (`PIPE_REJECT_REMOTE_CLIENTS`).

#[cfg(any(windows, test))]
use std::path::PathBuf;

#[cfg(windows)]
use futures::StreamExt;
#[cfg(windows)]
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
#[cfg(windows)]
use gpui::App;

/// The pipe of `user`'s Flux.
#[cfg(any(windows, test))]
fn pipe_name(user: &str) -> String {
    // A backslash would start a path inside the pipes' namespace.
    format!(r"\\.\pipe\flux-{}", user.replace('\\', "_"))
}

/// The paths as the pipe carries them: one per line — Windows forbids line breaks in file names.
#[cfg(any(windows, test))]
fn encode(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.to_string_lossy())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(any(windows, test))]
fn decode(message: &str) -> Vec<PathBuf> {
    message
        .lines()
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect()
}

#[cfg(windows)]
fn user_pipe() -> String {
    pipe_name(&std::env::var("USERNAME").unwrap_or_default())
}

/// Hands `paths` to the Flux that already runs; `false` when none does (this launch is Flux then).
/// The running Flux may come forward: this launch, which the user has just started, lets it.
#[cfg(windows)]
pub fn hand_over(paths: &[PathBuf]) -> bool {
    use std::io::Write;
    use windows_sys::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};

    const ERROR_PIPE_BUSY: i32 = 231;
    let name = user_pipe();
    // Every instance of the pipe is busy for a moment while the running Flux reads another launch.
    for _ in 0..20 {
        match std::fs::OpenOptions::new().write(true).open(&name) {
            Ok(mut pipe) => {
                // SAFETY: no pointers; it only allows another process to take the foreground.
                unsafe { AllowSetForegroundWindow(ASFW_ANY) };
                return pipe.write_all(encode(paths).as_bytes()).is_ok();
            }
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => return false,
        }
    }
    false
}

/// The paths later launches hand over: a thread waits for them on the pipe, for as long as Flux
/// runs; [`run`] opens them.
#[cfg(windows)]
pub fn listen() -> UnboundedReceiver<Vec<PathBuf>> {
    let (sender, receiver) = unbounded();
    if let Err(error) = std::thread::Builder::new()
        .name("flux-second-launch".into())
        .spawn(move || serve(&sender))
    {
        log::error!("second launch: no listener: {error}");
    }
    receiver
}

#[cfg(windows)]
fn serve(sender: &UnboundedSender<Vec<PathBuf>>) {
    use std::io::Read;
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_PIPE_CONNECTED, GetLastError, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_INBOUND;
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    let name: Vec<u16> = user_pipe().encode_utf16().chain(Some(0)).collect();
    loop {
        // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives the call; default
        // security (no attributes).
        let pipe = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_INBOUND,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                0,
                64 * 1024,
                0,
                std::ptr::null(),
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            log::error!("second launch: can't create the pipe");
            return;
        }
        // SAFETY: `pipe` is the pipe just created; no overlapped I/O. A launch that connected
        // between the creation and this call is connected too.
        let connected = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) } != 0
            || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
        if !connected {
            // SAFETY: the handle is ours and closed once.
            unsafe { CloseHandle(pipe) };
            continue;
        }
        // SAFETY: the handle is ours; the file owns it from here and closes it when dropped.
        let mut file = unsafe { std::fs::File::from_raw_handle(pipe) };
        let mut message = String::new();
        // The launch closes its end once it has written: the end of the message.
        if file.read_to_string(&mut message).is_ok()
            && sender.unbounded_send(decode(&message)).is_err()
        {
            return;
        }
    }
}

/// Opens what later launches hand over, as the folders and files macOS sends (`open_urls.rs`);
/// nothing — the window in front comes forward.
#[cfg(windows)]
pub fn run(mut launches: UnboundedReceiver<Vec<PathBuf>>, cx: &mut App) {
    cx.spawn(async move |cx| {
        while let Some(paths) = launches.next().await {
            let opened = cx.update(|cx| {
                if paths.is_empty() {
                    if let Some(front) = crate::open_urls::front_window(cx) {
                        front
                            .update(cx, |_, window, _| window.activate_window())
                            .ok();
                    }
                } else {
                    crate::open_urls::open(paths, cx);
                }
            });
            if opened.is_err() {
                return;
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pipe_is_the_users() {
        assert_eq!(pipe_name("egor"), r"\\.\pipe\flux-egor");
        assert_eq!(pipe_name(r"DOMAIN\egor"), r"\\.\pipe\flux-DOMAIN_egor");
        assert_ne!(pipe_name("anna"), pipe_name("egor"));
    }

    #[test]
    fn paths_go_through_the_pipe_unchanged() {
        let paths = vec![
            PathBuf::from(r"C:\Users\egor\My Project"),
            PathBuf::from(r"D:\проект\main.rs"),
        ];
        assert_eq!(decode(&encode(&paths)), paths);
        assert!(decode(&encode(&[])).is_empty());
        // A launch with nothing to open still says so: the window in front comes forward.
        assert_eq!(encode(&[]), "");
    }
}
