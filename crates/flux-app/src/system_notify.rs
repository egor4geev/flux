//! macOS notifications of Claude sessions (part 10.2): a session finished, or waits for an answer,
//! while Flux isn't the active app — the system tells (Settings → Claude Code → "Notify when a
//! session finishes or waits"), and a click on the notification brings Flux forward on that
//! session. Through `UNUserNotificationCenter`, which wants an app bundle: unbundled (`cargo run`,
//! UI scenarios) nothing is posted. The first notification asks macOS for the permission.
//!
//! On Windows (part 10.9) they are toasts under Flux's AppUserModelID, which its Start menu
//! shortcut carries: without the installed shortcut (`cargo run`) Windows shows none.
//!
//! Each session has one notification at a time (its identifier): "done" replaces "waits", an
//! answered question takes its notification back.

use futures::StreamExt;
use gpui::{App, Entity, EntityId};

use flux_claude::{EntryKind, Session};

use crate::claude_session::ClaudeSession;
use crate::claude_status::{self, GlanceState};
use crate::i18n::{tr, trf};

/// A notification's text at most (macOS cuts longer ones anyway).
const BODY_MAX_CHARS: usize = 200;

/// Listens for clicks on Flux's notifications: the session's window comes forward on it.
pub fn init(cx: &mut App) {
    let (clicks, mut received) = futures::channel::mpsc::unbounded::<u64>();
    // Unbundled, the sender is dropped at once and the listener ends.
    platform::install(clicks);
    cx.spawn(async move |cx| {
        while let Some(session) = received.next().await {
            cx.update(|cx| show_session(EntityId::from(session), cx))
                .ok();
        }
    })
    .detach();
}

/// A click on a notification: Flux comes forward, the window that has the session shows it.
fn show_session(session: EntityId, cx: &mut App) {
    cx.activate(true);
    for window in cx.windows() {
        window
            .update(cx, |_, window, cx| {
                window.dispatch_action(Box::new(crate::claude::ShowSession(session)), cx)
            })
            .ok();
    }
}

/// The session asks the user something (a permission, an edit, a question, its plan).
pub fn session_waits(session: &Entity<ClaudeSession>, cx: &App) {
    let model = session.read(cx).model();
    let glance = claude_status::glance(model);
    if glance.state != GlanceState::Waiting {
        return;
    }
    let body = glance.waiting.unwrap_or_default();
    post(session, tr("Waiting for your answer"), &body, cx);
}

/// The session finished its work (nothing runs in it any more after a turn).
pub fn session_finished(session: &Entity<ClaudeSession>, is_error: bool, cx: &App) {
    let model = session.read(cx).model();
    let subtitle = if is_error {
        tr("Stopped with an error")
    } else {
        tr("Done")
    };
    post(
        session,
        subtitle,
        &last_answer(model).unwrap_or_default(),
        cx,
    );
}

/// The question was answered (here or elsewhere): its notification goes.
pub fn withdraw(session: EntityId) {
    platform::withdraw(&identifier(session));
}

fn post(session: &Entity<ClaudeSession>, subtitle: &str, body: &str, cx: &App) {
    if !crate::settings::claude(cx).system_notifications || platform::app_is_active() {
        return;
    }
    let title = trf("Claude · {0}", &[&session.read(cx).title()]);
    platform::post(
        &identifier(session.entity_id()),
        session.entity_id().as_u64(),
        &title,
        subtitle,
        &clip(body, BODY_MAX_CHARS),
    );
}

/// One notification per session: a newer one replaces it.
fn identifier(session: EntityId) -> String {
    format!("dev.flux.claude.{}", session.as_u64())
}

/// The first line of Claude's last answer: what the "done" notification says.
fn last_answer(model: &Session) -> Option<String> {
    model
        .entries
        .iter()
        .rev()
        .find_map(|entry| match &entry.kind {
            EntryKind::Text { text, .. } => text
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(|line| line.trim_start_matches('#').trim().to_string()),
            EntryKind::User(_) => Some(String::new()),
            _ => None,
        })
        .filter(|line| !line.is_empty())
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut clipped: String = text.chars().take(max.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)] // `msg_send!` and `sel!` of objc 0.2 check a `cargo-clippy` feature.
mod platform {
    use std::ffi::CString;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, Once, OnceLock};

    use block::{Block, ConcreteBlock};
    use futures::channel::mpsc::UnboundedSender;
    use objc::declare::ClassDecl;
    use objc::rc::autoreleasepool;
    use objc::runtime::{BOOL, Class, NO, Object, Sel};
    use objc::{class, msg_send, sel, sel_impl};

    #[link(name = "UserNotifications", kind = "framework")]
    unsafe extern "C" {}

    type Id = *mut Object;

    /// UNAuthorizationOption: Sound | Alert.
    const AUTHORIZATION: usize = (1 << 1) | (1 << 2);
    /// UNNotificationPresentationOption: Sound | List | Banner.
    const PRESENTATION: usize = (1 << 1) | (1 << 3) | (1 << 4);
    const DELEGATE: &str = "FluxNotificationDelegate";

    /// Where clicks go: the listener of `init`.
    static CLICKS: OnceLock<Mutex<UnboundedSender<u64>>> = OnceLock::new();
    /// The permission was asked for in this process.
    static ASKED: AtomicBool = AtomicBool::new(false);

    /// The process is an app bundle: the notification center works only then (unbundled, it
    /// throws).
    fn available() -> bool {
        // SAFETY: plain Foundation calls; nil results are checked.
        unsafe {
            let bundle: Id = msg_send![class!(NSBundle), mainBundle];
            if bundle.is_null() {
                return false;
            }
            let identifier: Id = msg_send![bundle, bundleIdentifier];
            !identifier.is_null() && Class::get("UNUserNotificationCenter").is_some()
        }
    }

    fn center() -> Id {
        // SAFETY: called only when `available()`: the process has a bundle identifier.
        unsafe { msg_send![class!(UNUserNotificationCenter), currentNotificationCenter] }
    }

    fn string(text: &str) -> Id {
        let text = CString::new(text.replace('\0', "")).unwrap_or_default();
        // SAFETY: a NUL-terminated UTF-8 string; the result is autoreleased.
        unsafe { msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()] }
    }

    pub fn app_is_active() -> bool {
        // SAFETY: AppKit's shared application.
        unsafe {
            let app: Id = msg_send![class!(NSApplication), sharedApplication];
            let active: BOOL = msg_send![app, isActive];
            active != NO
        }
    }

    /// The center's delegate: clicks on Flux's notifications come to `clicks`.
    pub fn install(clicks: UnboundedSender<u64>) {
        if !available() {
            return;
        }
        if CLICKS.set(Mutex::new(clicks)).is_err() {
            return;
        }
        static REGISTER: Once = Once::new();
        REGISTER.call_once(|| {
            let Some(mut decl) = ClassDecl::new(DELEGATE, class!(NSObject)) else {
                return;
            };
            // SAFETY: the methods' signatures match the delegate protocol's: objects and blocks.
            unsafe {
                decl.add_method(
                    sel!(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:),
                    did_receive as extern "C" fn(&Object, Sel, Id, Id, Id),
                );
                decl.add_method(
                    sel!(userNotificationCenter:willPresentNotification:withCompletionHandler:),
                    will_present as extern "C" fn(&Object, Sel, Id, Id, Id),
                );
            }
            decl.register();
        });
        let Some(class) = Class::get(DELEGATE) else {
            return;
        };
        // SAFETY: a new delegate, kept for the process's life (the center holds it weakly).
        unsafe {
            let delegate: Id = msg_send![class, new];
            let _: () = msg_send![center(), setDelegate: delegate];
        }
    }

    /// A click on a notification (or its default action): its session goes to the listener. The
    /// completion handler is a block (`void (^)(void)`).
    extern "C" fn did_receive(_: &Object, _: Sel, _center: Id, response: Id, handler: Id) {
        let handler = handler as *mut Block<(), ()>;
        // SAFETY: the response and its notification come from the center; nil values are checked;
        // the completion handler must be called once.
        unsafe {
            let notification: Id = msg_send![response, notification];
            let request: Id = msg_send![notification, request];
            let content: Id = msg_send![request, content];
            let info: Id = msg_send![content, userInfo];
            if !info.is_null() {
                let number: Id = msg_send![info, objectForKey: string("session")];
                if !number.is_null() {
                    let session: u64 = msg_send![number, unsignedLongLongValue];
                    if let Some(Ok(clicks)) = CLICKS.get().map(|clicks| clicks.lock()) {
                        clicks.unbounded_send(session).ok();
                    }
                }
            }
            if !handler.is_null() {
                (*handler).call(());
            }
        }
    }

    /// Flux became active between posting and showing: the notification still shows. The
    /// completion handler is a block taking the presentation options.
    extern "C" fn will_present(_: &Object, _: Sel, _center: Id, _notification: Id, handler: Id) {
        let handler = handler as *mut Block<(usize,), ()>;
        // SAFETY: the completion handler takes the presentation options, called once.
        unsafe {
            if !handler.is_null() {
                (*handler).call((PRESENTATION,));
            }
        }
    }

    pub fn post(identifier: &str, session: u64, title: &str, subtitle: &str, body: &str) {
        if !available() {
            return;
        }
        let (identifier, title, subtitle, body) = (
            identifier.to_string(),
            title.to_string(),
            subtitle.to_string(),
            body.to_string(),
        );
        let send = move || deliver(&identifier, session, &title, &subtitle, &body);
        if ASKED.swap(true, Ordering::SeqCst) {
            return send();
        }
        // The first one in this process: macOS asks the user once (later it answers at once).
        let block = ConcreteBlock::new(move |granted: BOOL, _error: Id| {
            if granted != NO {
                send();
            }
        })
        .copy();
        let handler = &*block as *const Block<(BOOL, Id), ()> as Id;
        // SAFETY: the block is on the heap; the center copies it and calls it once.
        unsafe {
            let _: () = msg_send![center(), requestAuthorizationWithOptions: AUTHORIZATION
                completionHandler: handler];
        }
    }

    fn deliver(identifier: &str, session: u64, title: &str, subtitle: &str, body: &str) {
        autoreleasepool(|| {
            // SAFETY: UserNotifications objects built and handed to the center; `new` is
            // balanced by `release`.
            unsafe {
                let content: Id = msg_send![class!(UNMutableNotificationContent), new];
                let _: () = msg_send![content, setTitle: string(title)];
                if !subtitle.is_empty() {
                    let _: () = msg_send![content, setSubtitle: string(subtitle)];
                }
                if !body.is_empty() {
                    let _: () = msg_send![content, setBody: string(body)];
                }
                let sound: Id = msg_send![class!(UNNotificationSound), defaultSound];
                let _: () = msg_send![content, setSound: sound];
                let number: Id = msg_send![class!(NSNumber), numberWithUnsignedLongLong: session];
                let info: Id = msg_send![class!(NSDictionary),
                    dictionaryWithObject: number forKey: string("session")];
                let _: () = msg_send![content, setUserInfo: info];
                let _: () = msg_send![content, setThreadIdentifier: string(identifier)];
                let request: Id = msg_send![class!(UNNotificationRequest),
                    requestWithIdentifier: string(identifier)
                    content: content
                    trigger: std::ptr::null_mut::<Object>()];
                let _: () = msg_send![center(), addNotificationRequest: request
                    withCompletionHandler: std::ptr::null_mut::<Object>()];
                let _: () = msg_send![content, release];
            }
        });
    }

    pub fn withdraw(identifier: &str) {
        if !available() || !ASKED.load(Ordering::SeqCst) {
            return;
        }
        autoreleasepool(|| {
            // SAFETY: an array of one string for the center.
            unsafe {
                let identifiers: Id =
                    msg_send![class!(NSArray), arrayWithObject: string(identifier)];
                let _: () =
                    msg_send![center(), removeDeliveredNotificationsWithIdentifiers: identifiers];
            }
        });
    }
}

/// A toast's XML: the generic template, the title and up to two lines.
#[cfg(any(windows, test))]
fn toast_xml(title: &str, subtitle: &str, body: &str) -> String {
    let mut lines = String::new();
    for line in [title, subtitle, body] {
        if !line.is_empty() {
            lines.push_str(&format!("<text>{}</text>", escape_xml(line)));
        }
    }
    format!("<toast><visual><binding template=\"ToastGeneric\">{lines}</binding></visual></toast>")
}

/// Text for XML: its markup characters escaped, the control characters XML 1.0 has no place for
/// dropped.
#[cfg(any(windows, test))]
fn escape_xml(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            '\n' | '\t' | '\r' => escaped.push(c),
            c if c.is_control() => {}
            c => escaped.push(c),
        }
    }
    escaped
}

/// Windows' toasts, through WinRT. A click while Flux runs brings it forward on the session; a
/// toast clicked later in the notification center starts Flux through its shortcut, and the
/// second launch hands over to the running one.
#[cfg(windows)]
mod platform {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, OnceLock};

    use futures::channel::mpsc::UnboundedSender;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Foundation::TypedEventHandler;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
    use windows::core::{HSTRING, IInspectable};

    /// The AppUserModelID Flux sets at start, the one its Start menu shortcut carries: Windows
    /// shows the toasts of an app it knows by that id.
    const APP_ID: &str = "Flux.Flux";
    /// The sessions' toasts: one per session, by its tag in this group.
    const GROUP: &str = "claude";

    /// Where clicks go: the listener of `init`.
    static CLICKS: OnceLock<Mutex<UnboundedSender<u64>>> = OnceLock::new();
    /// The toasts shown, by tag: a click reaches a toast's handler only while the toast lives in
    /// this process.
    static SHOWN: Mutex<Vec<(String, ToastNotification)>> = Mutex::new(Vec::new());
    /// A failure was logged: without the shortcut every toast fails the same way.
    static FAILED: AtomicBool = AtomicBool::new(false);

    pub fn install(clicks: UnboundedSender<u64>) {
        CLICKS.set(Mutex::new(clicks)).ok();
    }

    /// One of Flux's windows is the foreground window.
    pub fn app_is_active() -> bool {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, GetWindowThreadProcessId,
        };
        let mut pid = 0;
        // SAFETY: reads the foreground window's process id into `pid`; a null window has none.
        unsafe {
            let window = GetForegroundWindow();
            !window.is_null()
                && GetWindowThreadProcessId(window, &mut pid) != 0
                && pid == std::process::id()
        }
    }

    /// A short tag: the session's number (older Windows 10 takes up to 16 characters).
    fn tag(identifier: &str) -> HSTRING {
        HSTRING::from(identifier.rsplit('.').next().unwrap_or(identifier))
    }

    pub fn post(identifier: &str, session: u64, title: &str, subtitle: &str, body: &str) {
        if let Err(error) = show(identifier, session, title, subtitle, body)
            && !FAILED.swap(true, Ordering::Relaxed)
        {
            log::warn!("system notification: {error}");
        }
    }

    fn show(
        identifier: &str,
        session: u64,
        title: &str,
        subtitle: &str,
        body: &str,
    ) -> windows::core::Result<()> {
        let document = XmlDocument::new()?;
        document.LoadXml(&HSTRING::from(super::toast_xml(title, subtitle, body)))?;
        let toast = ToastNotification::CreateToastNotification(&document)?;
        toast.SetTag(&tag(identifier))?;
        toast.SetGroup(&HSTRING::from(GROUP))?;
        toast.Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(
            move |_, _| {
                if let Some(Ok(clicks)) = CLICKS.get().map(|clicks| clicks.lock()) {
                    clicks.unbounded_send(session).ok();
                }
                Ok(())
            },
        ))?;
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))?
            .Show(&toast)?;
        if let Ok(mut shown) = SHOWN.lock() {
            shown.retain(|(shown, _)| shown != identifier);
            shown.push((identifier.to_string(), toast));
        }
        Ok(())
    }

    pub fn withdraw(identifier: &str) {
        let Ok(mut shown) = SHOWN.lock() else {
            return;
        };
        let count = shown.len();
        shown.retain(|(shown, _)| shown != identifier);
        if shown.len() == count {
            return;
        }
        ToastNotificationManager::History()
            .and_then(|history| {
                history.RemoveGroupedTagWithId(
                    &tag(identifier),
                    &HSTRING::from(GROUP),
                    &HSTRING::from(APP_ID),
                )
            })
            .ok();
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use futures::channel::mpsc::UnboundedSender;

    pub fn install(_: UnboundedSender<u64>) {}
    pub fn app_is_active() -> bool {
        true
    }
    pub fn post(_: &str, _: u64, _: &str, _: &str, _: &str) {}
    pub fn withdraw(_: &str) {}
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use flux_claude::Entry;

    use super::*;

    fn text(id: u64, text: &str) -> Entry {
        Entry {
            id,
            kind: EntryKind::Text {
                text: text.to_string(),
                streaming: false,
            },
        }
    }

    #[test]
    fn toasts_carry_escaped_text() {
        assert_eq!(
            toast_xml("Claude · <fix>", "", "Tom & \"Jerry\"\u{7}"),
            "<toast><visual><binding template=\"ToastGeneric\"><text>Claude · &lt;fix&gt;</text>\
             <text>Tom &amp; &quot;Jerry&quot;</text></binding></visual></toast>"
        );
    }

    #[test]
    fn the_done_notification_says_the_answer_s_first_line() {
        let mut model = Session::new(PathBuf::from("/project"));
        model.entries = vec![text(1, "## Fixed\n\nThe test passes now.")];
        assert_eq!(last_answer(&model).as_deref(), Some("Fixed"));
        model.entries.push(text(2, "\n  All green.  \nMore."));
        assert_eq!(last_answer(&model).as_deref(), Some("All green."));
        assert_eq!(clip("abcdef", 4), "abc…");
    }
}
