//! The computer for remote work with Claude (API 0.6, the bot's part 1): a plugin keeps it awake
//! while the user is away ([`set_awake`]), and hears when the screen locks or unlocks, the computer
//! goes to sleep or wakes, the power adapter comes or goes ([`RemoteEvent::System`]).
//!
//! Flux keeps the computer awake while any plugin asks — always, or only while on the adapter;
//! closing a laptop's lid still puts it to sleep. Nothing polls: the system tells Flux.
//!
//! - macOS: an IOKit power assertion (`PreventUserIdleSystemSleep`); the power sources' change
//!   notification; the screen lock's distributed notifications (`com.apple.screenIsLocked` /
//!   `…Unlocked`); the system power notifications of IOKit — going to sleep is acknowledged about
//!   two seconds late ([`SLEEP_HOLD`]), so a plugin's last request goes out.
//! - Windows: the thread execution state of the app's main thread (`SetThreadExecutionState`) and
//!   the power source read when asked (`GetSystemPowerStatus`); no lock, sleep or adapter events
//!   yet — they need a window's messages (`WTSRegisterSessionNotification`,
//!   `WM_POWERBROADCAST`).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use flux_plugin::claude_remote::{AwakeMode, RemoteEvent, SystemEvent, SystemState};
use futures::StreamExt;
use gpui::App;

/// Going to sleep waits this long for the plugins: their request about it goes out first.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const SLEEP_HOLD: Duration = Duration::from_secs(2);

/// What the system tells Flux (on the main thread's run loop): the event, and for the sleep the
/// notification to acknowledge.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
enum Signal {
    Event(SystemEvent),
    WillSleep(isize),
}

#[derive(Default)]
struct Power {
    /// What each plugin asked for.
    awake: HashMap<String, AwakeMode>,
    locked: bool,
    on_power: Option<bool>,
    /// The computer is held awake now.
    held: bool,
}

static POWER: Mutex<Option<Power>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut Power) -> T) -> T {
    let mut power = POWER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(power.get_or_insert_with(Power::default))
}

/// Starts listening to the system: the lock, the sleep, the power source.
pub(crate) fn init(cx: &mut App) {
    let (signals, mut received) = futures::channel::mpsc::unbounded::<Signal>();
    let locked = platform::watch(signals);
    with(|power| power.locked = locked);
    cx.spawn(async move |cx| {
        while let Some(signal) = received.next().await {
            match signal {
                Signal::Event(event) => {
                    cx.update(|_| on_event(event)).ok();
                }
                Signal::WillSleep(notification) => {
                    cx.update(|_| on_event(SystemEvent::WillSleep)).ok();
                    cx.background_executor().timer(SLEEP_HOLD).await;
                    platform::allow_sleep(notification);
                }
            }
        }
    })
    .detach();
}

fn on_event(event: SystemEvent) {
    let changed = with(|power| match event {
        SystemEvent::ScreenLocked => !std::mem::replace(&mut power.locked, true),
        SystemEvent::ScreenUnlocked => std::mem::replace(&mut power.locked, false),
        SystemEvent::Power(on) => {
            let changed = power.on_power != Some(on);
            power.on_power = Some(on);
            update(power);
            changed
        }
        SystemEvent::WillSleep | SystemEvent::DidWake => true,
    });
    if changed {
        crate::claude_hub::post(RemoteEvent::System(event));
    }
}

/// A plugin's wish (`claude-remote.set-awake`): Flux holds the computer awake while any plugin
/// asks. A plugin that stops asking (`Off`) lets it go, and one that stops running too (the hub
/// tells, [`crate::claude_hub`]).
pub(crate) fn set_awake(plugin: &str, mode: AwakeMode, _cx: &mut App) {
    with(|power| {
        match mode {
            AwakeMode::Off => power.awake.remove(plugin),
            mode => power.awake.insert(plugin.to_string(), mode),
        };
        update(power);
    });
}

/// The screen lock and the power source now.
pub(crate) fn state() -> SystemState {
    with(|power| SystemState {
        screen_locked: power.locked,
        on_power: on_power(power),
    })
}

fn on_power(power: &mut Power) -> bool {
    *power.on_power.get_or_insert_with(platform::on_power)
}

/// Holds the computer awake, or lets it go, as the plugins ask now.
fn update(power: &mut Power) {
    let adapter = on_power(power);
    let hold = holds(&power.awake, adapter);
    if hold != power.held {
        power.held = hold;
        platform::hold_awake(hold);
    }
}

/// Whether the plugins' wishes hold the computer awake: any `Always`, or any `OnPower` while on
/// the adapter.
fn holds(awake: &HashMap<String, AwakeMode>, on_power: bool) -> bool {
    let always = awake.values().any(|mode| *mode == AwakeMode::Always);
    let on_adapter = awake.values().any(|mode| *mode == AwakeMode::OnPower);
    always || on_adapter && on_power
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)] // `msg_send!` and `sel!` of objc 0.2 check a `cargo-clippy` feature.
mod platform {
    use std::ffi::{CString, c_void};
    use std::sync::Mutex;

    use block::ConcreteBlock;
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use core_foundation_sys::base::{CFRelease, CFTypeRef};
    use core_foundation_sys::dictionary::{CFDictionaryGetValueIfPresent, CFDictionaryRef};
    use core_foundation_sys::runloop::{
        CFRunLoopAddSource, CFRunLoopGetMain, CFRunLoopSourceRef, kCFRunLoopDefaultMode,
    };
    use core_foundation_sys::string::CFStringRef;
    use futures::channel::mpsc::UnboundedSender;
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    use super::{Signal, SystemEvent};

    type Id = *mut Object;
    type IoConnect = u32;
    type IoObject = u32;
    type IoNotificationPort = *mut c_void;

    /// `iokit_common_msg(…)` of `IOMessage.h`.
    const CAN_SYSTEM_SLEEP: u32 = 0xe000_0270;
    const SYSTEM_WILL_SLEEP: u32 = 0xe000_0280;
    const SYSTEM_HAS_POWERED_ON: u32 = 0xe000_0300;
    /// `kIOPMAssertionLevelOn`.
    const ASSERTION_ON: u32 = 255;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IORegisterForSystemPower(
            refcon: *mut c_void,
            port: *mut IoNotificationPort,
            callback: extern "C" fn(*mut c_void, IoObject, u32, *mut c_void),
            notifier: *mut IoObject,
        ) -> IoConnect;
        fn IONotificationPortGetRunLoopSource(port: IoNotificationPort) -> CFRunLoopSourceRef;
        fn IOAllowPowerChange(kernel_port: IoConnect, notification: isize) -> i32;
        fn IOPMAssertionCreateWithName(
            kind: CFStringRef,
            level: u32,
            name: CFStringRef,
            id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
        fn IOPSCopyPowerSourcesInfo() -> CFTypeRef;
        fn IOPSGetProvidingPowerSourceType(snapshot: CFTypeRef) -> CFStringRef;
        fn IOPSNotificationCreateRunLoopSource(
            callback: extern "C" fn(*mut c_void),
            context: *mut c_void,
        ) -> CFRunLoopSourceRef;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGSessionCopyCurrentDictionary() -> CFDictionaryRef;
    }

    /// Where the system's signals go, and the connection that acknowledges the sleep.
    static SIGNALS: Mutex<Option<UnboundedSender<Signal>>> = Mutex::new(None);
    static ROOT_PORT: Mutex<IoConnect> = Mutex::new(0);
    /// The assertion that holds the computer awake.
    static ASSERTION: Mutex<Option<u32>> = Mutex::new(None);

    fn send(signal: Signal) {
        if let Some(signals) = SIGNALS.lock().unwrap().as_ref() {
            let _ = signals.unbounded_send(signal);
        }
    }

    /// Registers for the system's notifications on the main run loop; whether the screen is
    /// locked now.
    pub(super) fn watch(signals: UnboundedSender<Signal>) -> bool {
        *SIGNALS.lock().unwrap() = Some(signals);
        // SAFETY: IOKit and Foundation registrations on the main thread; the sources and the
        // observers live as long as the app.
        unsafe {
            let mut port: IoNotificationPort = std::ptr::null_mut();
            let mut notifier: IoObject = 0;
            let root = IORegisterForSystemPower(
                std::ptr::null_mut(),
                &mut port,
                system_power,
                &mut notifier,
            );
            if root != 0 && !port.is_null() {
                *ROOT_PORT.lock().unwrap() = root;
                let source = IONotificationPortGetRunLoopSource(port);
                CFRunLoopAddSource(CFRunLoopGetMain(), source, kCFRunLoopDefaultMode);
            }
            let source = IOPSNotificationCreateRunLoopSource(power_sources, std::ptr::null_mut());
            if !source.is_null() {
                CFRunLoopAddSource(CFRunLoopGetMain(), source, kCFRunLoopDefaultMode);
            }
            let center: Id = msg_send![class!(NSDistributedNotificationCenter), defaultCenter];
            let queue: Id = msg_send![class!(NSOperationQueue), mainQueue];
            for (name, event) in [
                ("com.apple.screenIsLocked", SystemEvent::ScreenLocked),
                ("com.apple.screenIsUnlocked", SystemEvent::ScreenUnlocked),
            ] {
                let block = ConcreteBlock::new(move |_: Id| send(Signal::Event(event))).copy();
                let name = ns_string(name);
                let _: Id = msg_send![center, addObserverForName: name object: std::ptr::null_mut::<Object>()
                    queue: queue usingBlock: &*block];
            }
        }
        screen_locked()
    }

    fn ns_string(text: &str) -> Id {
        let text = CString::new(text).expect("no NUL");
        // SAFETY: a NUL-terminated UTF-8 string; the result is autoreleased.
        unsafe { msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()] }
    }

    /// The system power notifications: the sleep is acknowledged later ([`allow_sleep`]), idle
    /// sleep is allowed at once.
    extern "C" fn system_power(_: *mut c_void, _: IoObject, message: u32, argument: *mut c_void) {
        let notification = argument as isize;
        match message {
            CAN_SYSTEM_SLEEP => allow_sleep(notification),
            SYSTEM_WILL_SLEEP => send(Signal::WillSleep(notification)),
            SYSTEM_HAS_POWERED_ON => send(Signal::Event(SystemEvent::DidWake)),
            _ => {}
        }
    }

    extern "C" fn power_sources(_: *mut c_void) {
        send(Signal::Event(SystemEvent::Power(on_power())));
    }

    pub(super) fn allow_sleep(notification: isize) {
        let root = *ROOT_PORT.lock().unwrap();
        if root != 0 {
            // SAFETY: the connection of `IORegisterForSystemPower`, the system's notification id.
            unsafe { IOAllowPowerChange(root, notification) };
        }
    }

    /// On the power adapter (a desktop always is).
    pub(super) fn on_power() -> bool {
        // SAFETY: the snapshot is released; the type string belongs to it.
        unsafe {
            let snapshot = IOPSCopyPowerSourcesInfo();
            if snapshot.is_null() {
                return true;
            }
            let kind = IOPSGetProvidingPowerSourceType(snapshot);
            let battery = !kind.is_null() && CFString::wrap_under_get_rule(kind) == "Battery Power";
            CFRelease(snapshot);
            !battery
        }
    }

    /// Whether the screen is locked now (the session's `CGSSessionScreenIsLocked`).
    fn screen_locked() -> bool {
        // SAFETY: the dictionary is released; the key is a CFString of ours.
        unsafe {
            let session = CGSessionCopyCurrentDictionary();
            if session.is_null() {
                return false;
            }
            let key = CFString::from_static_string("CGSSessionScreenIsLocked");
            let mut value: *const c_void = std::ptr::null();
            let locked = CFDictionaryGetValueIfPresent(
                session,
                key.as_concrete_TypeRef() as *const c_void,
                &mut value,
            ) != 0;
            CFRelease(session as CFTypeRef);
            locked
        }
    }

    pub(super) fn hold_awake(hold: bool) {
        let mut assertion = ASSERTION.lock().unwrap();
        if hold && assertion.is_none() {
            let kind = CFString::from_static_string("PreventUserIdleSystemSleep");
            let name = CFString::from_static_string("Flux: remote work with Claude");
            let mut id = 0u32;
            // SAFETY: two CFStrings of ours and an out parameter.
            let result = unsafe {
                IOPMAssertionCreateWithName(
                    kind.as_concrete_TypeRef(),
                    ASSERTION_ON,
                    name.as_concrete_TypeRef(),
                    &mut id,
                )
            };
            if result == 0 {
                *assertion = Some(id);
            }
        } else if !hold && let Some(id) = assertion.take() {
            // SAFETY: an assertion this process created.
            unsafe { IOPMAssertionRelease(id) };
        }
    }
}

#[cfg(windows)]
mod platform {
    use futures::channel::mpsc::UnboundedSender;
    use windows_sys::Win32::System::Power::{
        ES_CONTINUOUS, ES_SYSTEM_REQUIRED, GetSystemPowerStatus, SYSTEM_POWER_STATUS,
        SetThreadExecutionState,
    };

    use super::Signal;

    /// No lock, sleep or adapter events yet (they need a window's messages); the screen counts as
    /// unlocked.
    pub(super) fn watch(_: UnboundedSender<Signal>) -> bool {
        false
    }

    pub(super) fn allow_sleep(_: isize) {}

    /// On the power adapter: `ACLineStatus` 1 (a desktop without a battery says so too).
    pub(super) fn on_power() -> bool {
        let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
        // SAFETY: an out parameter of the right size.
        let read = unsafe { GetSystemPowerStatus(&mut status) } != 0;
        !read || status.ACLineStatus != 0
    }

    /// The main thread's execution state: it holds the system awake until it is cleared (the
    /// calls come from the app's main thread, which lives as long as Flux).
    pub(super) fn hold_awake(hold: bool) {
        let state = if hold {
            ES_CONTINUOUS | ES_SYSTEM_REQUIRED
        } else {
            ES_CONTINUOUS
        };
        // SAFETY: a plain flag call.
        unsafe { SetThreadExecutionState(state) };
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use futures::channel::mpsc::UnboundedSender;

    use super::Signal;

    pub(super) fn watch(_: UnboundedSender<Signal>) -> bool {
        false
    }

    pub(super) fn allow_sleep(_: isize) {}

    pub(super) fn on_power() -> bool {
        true
    }

    pub(super) fn hold_awake(_: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_computer_is_held_while_any_plugin_asks() {
        let mut awake = HashMap::new();
        assert!(!holds(&awake, true));
        awake.insert("a".to_string(), AwakeMode::OnPower);
        assert!(
            !holds(&awake, false),
            "on the battery, OnPower doesn't hold"
        );
        assert!(holds(&awake, true));
        awake.insert("b".to_string(), AwakeMode::Always);
        assert!(holds(&awake, false), "Always holds on the battery too");
    }
}
