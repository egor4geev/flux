//! The API 0.5's two more places of a window (FluxGram part 2), with fake accounts: a block of the
//! plugin's Settings page (`location = "settings"`) — two-line rows with avatars and a Sign Out
//! button, Add Account under them — and a dialog (`location = "dialog"`) that Add Account opens: a
//! QR code with the steps and a countdown, or a phone number, a code and a password
//! (`password_field`), one screen after another in the same dialog; the last one closes it and the
//! list gets a new row. Pauses stand for the server: the QR code's placeholder (`qr("")`) until the
//! code comes, and a busy Next (`busy`) while the answer is awaited. Nothing is sent anywhere.

use std::time::Duration;

use flux_plugin_api::conversation::AvatarSpec;
use flux_plugin_api::host::ui;
use flux_plugin_api::view::*;
use flux_plugin_api::{Event, UiEvent, timers, tr, trf};

/// The block of the Settings page.
pub(crate) const ACCOUNTS: &str = "accounts";
/// The sign-in dialog.
pub(crate) const SIGN_IN: &str = "sign-in";
/// How long a QR code lives before a new one replaces it.
const QR_LIFETIME: u32 = 30;
/// The pauses that stand for the server: the code fetched, Next answered.
const CODE_WAIT: Duration = Duration::from_millis(1500);
const NEXT_WAIT: Duration = Duration::from_millis(1200);

/// What the dialog waits for (a timer stands for the server).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Wait {
    /// The QR code: its placeholder until then.
    Code,
    /// The answer to Next: the button is busy until then.
    Next,
}

/// A fake account.
struct Account {
    key: String,
    name: String,
    phone: String,
    label: String,
    color: u8,
}

/// The dialog's screen.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Step {
    #[default]
    Qr,
    Phone,
    Code,
    Password,
}

/// The accounts and the sign-in in progress.
pub(crate) struct State {
    accounts: Vec<Account>,
    next: u32,
    step: Step,
    /// The QR code's text and the seconds it has left; the timer that counts them.
    token: u32,
    left: u32,
    timer: Option<u64>,
    /// The pause in progress and its timer.
    wait: Option<(u64, Wait)>,
    /// What is typed into the dialog's fields, by field.
    phone: String,
    code: String,
    password: String,
    error: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        State {
            accounts: vec![
                Account {
                    key: "1".into(),
                    name: "Egor Ageev".into(),
                    phone: "+7 ••• •••-••-12".into(),
                    label: "Personal".into(),
                    color: 4,
                },
                Account {
                    key: "2".into(),
                    name: "Egor · Work".into(),
                    phone: "+7 ••• •••-••-45".into(),
                    label: "Work".into(),
                    color: 2,
                },
            ],
            next: 3,
            step: Step::Qr,
            token: 1,
            left: QR_LIFETIME,
            timer: None,
            wait: None,
            phone: String::new(),
            code: String::new(),
            password: String::new(),
            error: None,
        }
    }
}

impl State {
    /// The accounts' block, set at the start and on every change: the page shows it at once.
    pub(crate) fn render_accounts(&self) {
        let mut children = Vec::new();
        if self.accounts.is_empty() {
            children.push(text(
                "accounts-none",
                [span(&tr("No accounts yet.")).tone(Tone::Dim)],
            ));
        } else {
            let mut tree = Tree::new().two_line();
            for account in &self.accounts {
                let initials: String = account.name.chars().take(1).collect();
                tree.add(
                    RowSpec::new(&account.key, &account.name)
                        .avatar(AvatarSpec::initials(&initials, account.color))
                        .subtitle(vec![span(&format!(
                            "{} · {}",
                            account.phone,
                            tr(&account.label)
                        ))])
                        .action("sign-out", Some(&tr("Sign Out")), None, false),
                );
            }
            children.push(tree.into_element("accounts-list"));
        }
        children.push(row(
            "accounts-buttons",
            [button("accounts-add", &tr("Add Account"))
                .icon("plus")
                .primary()
                .into()],
        ));
        set_view(ACCOUNTS, column("accounts-root", children));
    }

    /// The dialog's screen as it is now.
    fn render_dialog(&self) {
        let mut children = Vec::new();
        match self.step {
            // Until the code comes: its placeholder, the same size.
            Step::Qr if self.waits(Wait::Code) => {
                children.push(qr("sign-in-qr", ""));
                children.push(text(
                    "sign-in-countdown",
                    [span(&tr("Getting the code…")).tone(Tone::Dim)],
                ));
                children.push(row(
                    "sign-in-buttons",
                    [
                        button("sign-in-phone", &tr("Use Phone Number")).into(),
                        spacer("sign-in-gap"),
                        button("sign-in-cancel", &tr("Cancel")).into(),
                    ],
                ));
            }
            Step::Qr => {
                children.push(qr(
                    "sign-in-qr",
                    &format!("tg://login?token=playground-{}", self.token),
                ));
                children.push(markdown(
                    "sign-in-steps",
                    &tr("1. Open the messenger on your phone\n2. Settings → Devices → Link Desktop Device\n3. Point the camera at the code"),
                ));
                let left = self.left.to_string();
                children.push(text(
                    "sign-in-countdown",
                    [span(&trf("The code refreshes in {0} s", &[&left])).tone(Tone::Dim)],
                ));
                children.push(row(
                    "sign-in-buttons",
                    [
                        button("sign-in-phone", &tr("Use Phone Number")).into(),
                        spacer("sign-in-gap"),
                        button("sign-in-cancel", &tr("Cancel")).into(),
                    ],
                ));
            }
            Step::Phone | Step::Code | Step::Password => {
                let (about, field) = match self.step {
                    Step::Phone => (
                        tr("Your phone number with the country code."),
                        text_field("sign-in-phone-field", &self.phone, Some("+7 900 000-00-00")),
                    ),
                    Step::Code => (
                        trf(
                            "The code sent to {0}. Any five digits will do.",
                            &[&self.phone],
                        ),
                        text_field("sign-in-code-field", &self.code, Some(&tr("Code"))),
                    ),
                    _ => (
                        tr("The account has a cloud password. Any one will do."),
                        password_field(
                            "sign-in-password-field",
                            &self.password,
                            Some(&tr("Password")),
                        ),
                    ),
                };
                children.push(text("sign-in-about", [span(&about).tone(Tone::Dim)]));
                children.push(field);
                if let Some(error) = &self.error {
                    children.push(text("sign-in-error", [span(error).tone(Tone::Error)]));
                }
                children.push(row(
                    "sign-in-buttons",
                    [
                        button("sign-in-back", &tr("Back")).into(),
                        spacer("sign-in-gap"),
                        button("sign-in-cancel", &tr("Cancel")).into(),
                        button("sign-in-next", &tr("Next"))
                            .primary()
                            .busy(self.waits(Wait::Next))
                            .into(),
                    ],
                ));
            }
        }
        set_view(SIGN_IN, column("sign-in-root", children));
    }

    /// The dialog waits for this.
    fn waits(&self, what: Wait) -> bool {
        self.wait.is_some_and(|(_, wait)| wait == what)
    }

    /// A pause standing for the server; one at a time.
    fn wait_for(&mut self, what: Wait) {
        self.cancel_wait();
        let pause = match what {
            Wait::Code => CODE_WAIT,
            Wait::Next => NEXT_WAIT,
        };
        self.wait = Some((timers::after(pause), what));
    }

    /// The user went elsewhere: what was awaited doesn't matter any more.
    fn cancel_wait(&mut self) {
        if let Some((timer, _)) = self.wait.take() {
            timers::cancel(timer);
        }
    }

    /// Add Account: the dialog opens on the QR code's placeholder, then the code counts down.
    fn start(&mut self) {
        self.wait_for(Wait::Code);
        self.step = Step::Qr;
        self.phone.clear();
        self.code.clear();
        self.password.clear();
        self.error = None;
        self.left = QR_LIFETIME;
        if self.timer.is_none() {
            self.timer = Some(timers::every(Duration::from_secs(1)));
        }
        self.render_dialog();
        ui::show(SIGN_IN);
    }

    /// The dialog closed (done, Cancel, Esc, ×): the countdown and the pause stop.
    fn stop(&mut self) {
        for timer in [self.timer.take(), self.wait.take().map(|(timer, _)| timer)]
            .into_iter()
            .flatten()
        {
            timers::cancel(timer);
        }
    }

    /// ↵ in a field or Next: what is typed is checked, and Next is busy while "the server" answers;
    /// pressed again meanwhile (↵ in the field), nothing happens.
    fn next(&mut self) {
        if self.wait.is_some() {
            return;
        }
        self.error = Some(match self.step {
            Step::Qr => return,
            Step::Phone if self.phone.chars().filter(char::is_ascii_digit).count() < 7 => {
                tr("That doesn't look like a phone number.")
            }
            Step::Code if self.code.trim().len() != 5 => tr("The code has five digits."),
            Step::Password if self.password.is_empty() => tr("Type the password."),
            _ => {
                self.error = None;
                self.wait_for(Wait::Next);
                self.render_dialog();
                return;
            }
        });
        self.render_dialog();
    }

    /// "The server" answered Next: the next screen, or the new account.
    fn advance(&mut self) {
        match self.step {
            Step::Qr => {}
            Step::Phone => self.step = Step::Code,
            Step::Code => self.step = Step::Password,
            Step::Password => {
                let number = self.next;
                self.next += 1;
                let digits: String = self.phone.chars().filter(char::is_ascii_digit).collect();
                let tail = &digits[digits.len().saturating_sub(2)..];
                self.accounts.push(Account {
                    key: number.to_string(),
                    name: trf("Account {0}", &[&number]),
                    phone: format!("+{} ••• •••-••-{tail}", &digits[..1]),
                    label: "New".into(),
                    color: (number % 7) as u8,
                });
                self.render_accounts();
                ui::hide(SIGN_IN);
                return;
            }
        }
        self.render_dialog();
    }

    /// The text of one of the dialog's fields.
    fn typed(&mut self, field: &str, text: String) {
        match field {
            "sign-in-phone-field" => self.phone = text,
            "sign-in-code-field" => self.code = text,
            "sign-in-password-field" => self.password = text,
            _ => {}
        }
    }

    /// The events of the two windows: handled here (none), or handed back.
    pub(crate) fn handle(&mut self, event: Event) -> Option<Event> {
        match event {
            Event::ToolWindowShown(window) if window == ACCOUNTS => self.render_accounts(),
            Event::ToolWindowHidden(window) if window == SIGN_IN => self.stop(),
            Event::Timer(id) if self.wait.is_some_and(|(timer, _)| timer == id) => {
                match self.wait.take().map(|(_, what)| what) {
                    Some(Wait::Next) => self.advance(),
                    _ => {
                        self.left = QR_LIFETIME;
                        self.render_dialog();
                    }
                }
            }
            Event::Timer(id) if Some(id) == self.timer => {
                self.left = self.left.saturating_sub(1);
                if self.left == 0 {
                    self.token += 1;
                    self.left = QR_LIFETIME;
                }
                if self.step == Step::Qr {
                    self.render_dialog();
                }
            }
            Event::Ui(input) if input.window == ACCOUNTS => match input.event {
                UiEvent::Clicked if input.element == "accounts-add" => self.start(),
                UiEvent::RowAction((key, action)) if action == "sign-out" => {
                    self.accounts.retain(|account| account.key != key);
                    self.render_accounts();
                }
                _ => {}
            },
            Event::Ui(input) if input.window == SIGN_IN => {
                match (input.element.as_str(), input.event) {
                    ("sign-in-phone", UiEvent::Clicked) => {
                        self.cancel_wait();
                        self.step = Step::Phone;
                        self.render_dialog();
                    }
                    ("sign-in-back", UiEvent::Clicked) => {
                        self.cancel_wait();
                        self.error = None;
                        self.step = match self.step {
                            Step::Password => Step::Code,
                            Step::Code => Step::Phone,
                            _ => Step::Qr,
                        };
                        self.render_dialog();
                    }
                    ("sign-in-cancel", UiEvent::Clicked) => ui::hide(SIGN_IN),
                    ("sign-in-next", UiEvent::Clicked) => self.next(),
                    (field, UiEvent::Changed(text)) => self.typed(field, text),
                    // ↵ in a field: what it has, and on.
                    (field, UiEvent::Submitted(text)) => {
                        self.typed(field, text);
                        self.next();
                    }
                    _ => {}
                }
            }
            other => return Some(other),
        }
        None
    }
}
