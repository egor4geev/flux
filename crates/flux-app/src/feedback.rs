//! Feedback (part 10.6), as JetBrains' Submit a Bug Report: Help → Report a Bug… opens the bug form
//! of Flux's repository on GitHub with this Flux and this Mac already filled in, Suggest a Feature…
//! and Ask a Question… open its Discussions; a plugin's Report an Issue goes where the plugin lives
//! (`flux-plugins` for the catalog's plugins, the plugin's own repository otherwise). About → Copy
//! takes the same facts ([`SystemInfo`]), as JetBrains' Copy and Close.
//!
//! The forms' fields are filled through the URL's query, by their ids ([`field`]): the forms in
//! `.github/` of both repositories use exactly these ids. GitHub documents this for issue forms
//! only: the Discussions' forms get the version (and macOS for a question) the same way, and may
//! leave them empty.

use std::sync::OnceLock;

use flux_plugin::registry::PluginSource;
use gpui::{AnyElement, App, Entity, actions, div, prelude::*};

use crate::app_log::Crash;
use crate::i18n::{self, tr, trf};
use crate::plugin_links::open_url;
use crate::plugins::{PluginStatus, PluginStore};
use crate::theme::UiColors;

actions!(
    help,
    [
        /// Help → Report a Bug…: the bug form on GitHub, with the version, macOS, the processor
        /// and the plugins filled in.
        ReportBug,
        /// Help → Suggest a Feature…: a new idea in Discussions.
        SuggestFeature,
        /// Help → Ask a Question…: a new question in Discussions (Q&A).
        AskQuestion,
    ]
);

/// Flux's repository: its Issues and Discussions.
pub const REPOSITORY: &str = "https://github.com/egor4geev/flux";
/// The catalog's repository: the issues of its plugins.
pub const PLUGINS_REPOSITORY: &str = "https://github.com/egor4geev/flux-plugins";

/// The forms on GitHub and the ids of their fields.
pub mod field {
    /// Flux's bug form: `.github/ISSUE_TEMPLATE/bug_report.yml` of `flux`.
    pub const BUG_TEMPLATE: &str = "bug_report.yml";
    /// A catalog plugin's bug form: `.github/ISSUE_TEMPLATE/plugin_bug.yml` of `flux-plugins`.
    pub const PLUGIN_BUG_TEMPLATE: &str = "plugin_bug.yml";
    /// The Discussions categories (their slugs; a category's form is
    /// `.github/DISCUSSION_TEMPLATE/<slug>.yml` of `flux`).
    pub const IDEAS: &str = "ideas";
    pub const QUESTIONS: &str = "q-a";

    /// What happened (a textarea; a crash report fills it with the panic's message).
    pub const WHAT_HAPPENED: &str = "what-happened";
    // The two fields only the reporter writes: here for the forms to keep these ids.
    /// The steps to reproduce (a textarea).
    #[allow(dead_code)]
    pub const STEPS: &str = "steps";
    /// What was expected instead (a textarea).
    #[allow(dead_code)]
    pub const EXPECTED: &str = "expected";
    /// Flux's version: "0.1.0 (ca086c2)" (an input).
    pub const VERSION: &str = "version";
    /// macOS: "macOS 26.1 (25B78)" (an input).
    pub const MACOS: &str = "macos";
    /// The processor: "Apple M3 Pro, arm64" (an input).
    pub const HARDWARE: &str = "hardware";
    /// The installed plugins, one per line: "flux.rust 0.1.0" (a textarea).
    pub const PLUGINS: &str = "plugins";
    /// The log or the crash report (a textarea, shown as code).
    pub const LOGS: &str = "logs";
    /// The plugin of a plugin's report: "Yandex Tracker & Calendar (flux.yandex)" (an input).
    pub const PLUGIN: &str = "plugin";
    /// The plugin's version (an input).
    pub const PLUGIN_VERSION: &str = "plugin-version";
}

/// The longest address a report opens. GitHub answers 414 to request lines past about 8 KB; the
/// crash's stack is cut first, then the plugins.
const URL_BUDGET: usize = 7 * 1024;
/// What ends a value cut to the budget.
const CUT_MARK: &str = "\n…";
/// The longest crash message in an issue's title.
const TITLE_CHARS: usize = 72;

/// What a report says about this Flux and this Mac.
#[derive(Debug, Clone, Default)]
pub struct SystemInfo {
    /// [`build_version`].
    pub version: String,
    /// "macOS 26.1 (25B78)".
    pub macos: String,
    /// "Apple M3 Pro, arm64, 36 GB"; under Rosetta "…, x86_64 (Rosetta), …".
    pub hardware: String,
    /// The interface language: "English", "Русский"; empty in [`machine`].
    pub language: String,
    /// The plugins that aren't built in, by id, with their versions: ("flux.rust", "0.1.0"); one
    /// under development or turned off says so after its version ("0.1.0, development").
    pub plugins: Vec<(String, String)>,
}

impl SystemInfo {
    /// The facts as lines of English text, as JetBrains' Copy and Close: About → Copy, the top of
    /// a crash report.
    pub fn to_text(&self) -> String {
        let mut lines = vec![format!("Flux {}", self.version)];
        lines.extend(
            [&self.macos, &self.hardware]
                .into_iter()
                .filter(|line| !line.is_empty())
                .cloned(),
        );
        if !self.language.is_empty() {
            lines.push(format!("UI language: {}", self.language));
        }
        if !self.plugins.is_empty() {
            lines.push("Plugins:".to_string());
            lines.extend(self.plugin_lines().map(|line| format!("    {line}")));
        }
        lines.join("\n")
    }

    /// The plugins as the bug form's field takes them: one per line.
    fn plugins_field(&self) -> String {
        if self.plugins.is_empty() {
            return "No plugins besides the built-in ones".to_string();
        }
        self.plugin_lines().collect::<Vec<_>>().join("\n")
    }

    fn plugin_lines(&self) -> impl Iterator<Item = String> + '_ {
        self.plugins
            .iter()
            .map(|(id, version)| format!("{id} {version}"))
    }
}

/// Flux's version and the commit it is built from: "0.1.0 (ca086c2)"; "0.1.0" for a build outside
/// git.
pub fn build_version() -> String {
    let version = env!("CARGO_PKG_VERSION");
    match env!("FLUX_COMMIT") {
        "" => version.to_string(),
        commit => format!("{version} ({commit})"),
    }
}

/// The facts about this Flux and this Mac that don't need the app: the version, macOS and the
/// processor (the log's header and a crash report, written from a panic hook, take them). Read once.
pub fn machine() -> SystemInfo {
    let mac = mac_facts();
    let mut hardware = mac.processor.clone();
    if let Some(gb) = mac.memory_gb {
        hardware.push_str(&format!(", {gb} GB"));
    }
    SystemInfo {
        version: build_version(),
        macos: mac.system.clone(),
        hardware,
        ..SystemInfo::default()
    }
}

/// The computer as About shows it: the processor and the memory, its unit in the interface
/// language ("Apple M3 Pro, arm64, 36 ГБ").
pub fn computer() -> String {
    let mac = mac_facts();
    match mac.memory_gb {
        Some(gb) => format!("{}, {}", mac.processor, trf("{0} GB", &[&gb])),
        None => mac.processor.clone(),
    }
}

/// What `sysctl` says about this Mac, read once.
struct MacFacts {
    /// "macOS 26.1 (25B78)".
    system: String,
    /// "Apple M3 Pro, arm64".
    processor: String,
    memory_gb: Option<u64>,
}

fn mac_facts() -> &'static MacFacts {
    static FACTS: OnceLock<MacFacts> = OnceLock::new();
    FACTS.get_or_init(|| MacFacts {
        system: mac::system(),
        processor: mac::processor(),
        memory_gb: mac::memory_gb(),
    })
}

/// All the facts: [`machine`], the interface language and the plugins — of the active window's
/// store. A window that is being updated can't be read: from inside one (a click in it) take
/// [`system_info_with`], or defer.
pub fn system_info(cx: &App) -> SystemInfo {
    let plugins = crate::settings_file::workspace_window(cx)
        .and_then(|window| window.read(cx).ok())
        .map(|workspace| workspace.plugins.clone());
    with_plugins(plugins.as_ref(), cx)
}

/// [`system_info`] with the plugins of a given store (a window's own views).
pub fn system_info_with(plugins: &Entity<PluginStore>, cx: &App) -> SystemInfo {
    with_plugins(Some(plugins), cx)
}

fn with_plugins(plugins: Option<&Entity<PluginStore>>, cx: &App) -> SystemInfo {
    let mut info = machine();
    info.language = i18n::lang_name(i18n::lang()).to_string();
    if let Some(plugins) = plugins {
        info.plugins = plugin_list(plugins.read(cx));
    }
    info
}

/// The plugins that aren't built in, sorted by id.
fn plugin_list(store: &PluginStore) -> Vec<(String, String)> {
    let mut plugins: Vec<(String, String)> = store
        .plugins()
        .iter()
        .filter(|plugin| plugin.source() != PluginSource::Bundled)
        .map(|plugin| {
            let mut version = plugin.entry.manifest.version.clone();
            if plugin.source() == PluginSource::Dev {
                version.push_str(", development");
            }
            if plugin.status == PluginStatus::Disabled {
                version.push_str(", disabled");
            }
            (plugin.id().to_string(), version)
        })
        .collect();
    plugins.sort();
    plugins
}

/// Opens the bug form with this Flux and this Mac filled in; with a crash, its message and stack
/// too.
pub fn report_bug(crash: Option<&Crash>, cx: &mut App) {
    let crash = crash.cloned();
    // A menu item or a button is handled while its window is updated: the window's plugins can
    // be read once that is over.
    cx.defer(move |cx| {
        let url = bug_report_url(&system_info(cx), crash.as_ref());
        open_url(&url, cx);
    });
}

/// The bug form's address: the facts filled in; a crash's version, message and report.
pub fn bug_report_url(info: &SystemInfo, crash: Option<&Crash>) -> String {
    let mut params = Vec::new();
    let mut version = info.version.clone();
    if let Some(crash) = crash {
        params.push(("title", crash_title(crash)));
        params.push((
            field::WHAT_HAPPENED,
            format!("Flux crashed: {}", crash.message.trim()),
        ));
        params.push((field::LOGS, crash_text(crash)));
        // The crash is the bug: the version that crashed (Flux may have been updated since).
        if !crash.version.is_empty() {
            version = crash.version.clone();
        }
    }
    params.extend([
        (field::VERSION, version),
        (field::MACOS, info.macos.clone()),
        (field::HARDWARE, info.hardware.clone()),
        (field::PLUGINS, info.plugins_field()),
    ]);
    issue_url(REPOSITORY, field::BUG_TEMPLATE, params)
}

/// "Crash: <the first line of the message>", short enough for a title.
fn crash_title(crash: &Crash) -> String {
    let first = crash.message.lines().next().unwrap_or_default().trim();
    let mut title: String = first.chars().take(TITLE_CHARS).collect();
    if first.chars().count() > TITLE_CHARS {
        title.push('…');
    }
    if title.is_empty() {
        "Crash".to_string()
    } else {
        format!("Crash: {title}")
    }
}

/// The crash as the form's log field shows it: when and where, the message, the stack.
fn crash_text(crash: &Crash) -> String {
    let mut text = String::new();
    if !crash.time.is_empty() {
        text.push_str(&format!("{}\n", crash.time));
    }
    let thread = if crash.thread.is_empty() {
        "<unnamed>"
    } else {
        &crash.thread
    };
    text.push_str(&format!("thread '{thread}' panicked"));
    if !crash.location.is_empty() {
        text.push_str(&format!(" at {}", crash.location));
    }
    text.push_str(&format!(":\n{}", crash.message.trim_end()));
    let backtrace = crash.backtrace.trim_end();
    if !backtrace.is_empty() {
        text.push_str(&format!("\n\n{backtrace}"));
    }
    text
}

/// A Discussions category's form: `discussions/new?category=<slug>`, with `fields` filled if GitHub
/// takes them (undocumented for discussions).
fn discussion_url(category: &str, fields: Vec<(&str, String)>) -> String {
    let mut params = vec![("category", category.to_string())];
    params.extend(fields);
    with_query(&format!("{REPOSITORY}/discussions/new"), &params)
}

/// Suggest a Feature…: a new idea, Flux's version in its form.
fn idea_url(info: &SystemInfo) -> String {
    discussion_url(field::IDEAS, vec![(field::VERSION, info.version.clone())])
}

/// Ask a Question…: a new question, the version and macOS in its form.
fn question_url(info: &SystemInfo) -> String {
    discussion_url(
        field::QUESTIONS,
        vec![
            (field::VERSION, info.version.clone()),
            (field::MACOS, info.macos.clone()),
        ],
    )
}

/// A plugin a report is about: what its Report an Issue needs.
#[derive(Debug, Clone, Default)]
pub struct PluginReport {
    pub id: String,
    /// Its name in English (the report is read on GitHub).
    pub name: String,
    pub version: String,
    /// The manifest's `repository`.
    pub repository: Option<String>,
    /// Shipped inside Flux: its problems are Flux's.
    pub bundled: bool,
}

impl PluginReport {
    /// Whether the plugin has somewhere to report to: built in, or with a repository.
    pub fn reportable(&self) -> bool {
        self.bundled || self.repository.is_some()
    }
}

/// Where a plugin's problems go: a built-in plugin's (or one of Flux's own repository) — Flux's
/// bug form, the plugin named in it; a catalog plugin's — `flux-plugins`' plugin form with the
/// plugin and its version filled in; a plugin of another GitHub repository — its new issue; any
/// other address as it is; without a repository — nowhere.
pub fn plugin_issue_url(plugin: &PluginReport, info: &SystemInfo) -> Option<String> {
    let repository = plugin.repository.as_deref().map(normalize_repository);
    let named = format!("{} ({})", plugin.name, plugin.id);
    if plugin.bundled || repository.as_deref() == Some(REPOSITORY) {
        let params = vec![
            (
                field::WHAT_HAPPENED,
                format!("The plugin {named} {}:\n\n", plugin.version),
            ),
            (field::VERSION, info.version.clone()),
            (field::MACOS, info.macos.clone()),
            (field::HARDWARE, info.hardware.clone()),
            (field::PLUGINS, info.plugins_field()),
        ];
        return Some(issue_url(REPOSITORY, field::BUG_TEMPLATE, params));
    }
    let repository = repository?;
    if repository == PLUGINS_REPOSITORY {
        let params = vec![
            (field::PLUGIN, named),
            (field::PLUGIN_VERSION, plugin.version.clone()),
            (field::VERSION, info.version.clone()),
            (field::MACOS, info.macos.clone()),
            (field::HARDWARE, info.hardware.clone()),
        ];
        return Some(issue_url(
            PLUGINS_REPOSITORY,
            field::PLUGIN_BUG_TEMPLATE,
            params,
        ));
    }
    if github_repository(&repository) {
        return Some(format!("{repository}/issues/new"));
    }
    let web = repository.starts_with("https://") || repository.starts_with("http://");
    web.then_some(repository)
}

/// Opens [`plugin_issue_url`] in the browser.
pub fn report_plugin_issue(plugin: &PluginReport, cx: &mut App) {
    let plugin = plugin.clone();
    // As `report_bug`: the click comes while its window is updated.
    cx.defer(move |cx| {
        if let Some(url) = plugin_issue_url(&plugin, &system_info(cx)) {
            open_url(&url, cx);
        }
    });
}

/// «Report an Issue»: a link among the facts of a plugin's Overview (Settings → Plugins, the
/// Marketplace), as JetBrains' plugin pages list the bug tracker; `None` when the plugin has nowhere
/// to report to.
pub fn report_issue_link(
    id: &'static str,
    plugin: PluginReport,
    ui: UiColors,
) -> Option<AnyElement> {
    plugin.reportable().then(|| {
        div()
            .id(id)
            .text_color(ui.accent_text)
            .cursor_pointer()
            .hover(|style| style.underline())
            .on_click(move |_, _, cx| report_plugin_issue(&plugin, cx))
            .child(tr("Report an Issue"))
            .into_any_element()
    })
}

/// A repository's address as the reports compare it: `https://github.com/owner/repo`, without a
/// trailing `/` or `.git`, a GitHub repository without the path past its name (`/tree/main/x`); an
/// SSH address (`git@github.com:owner/repo.git`) becomes the web one.
fn normalize_repository(repository: &str) -> String {
    let repository = repository.trim();
    let repository = match repository.strip_prefix("git@github.com:") {
        Some(path) => format!("https://github.com/{path}"),
        None => repository.replacen("http://github.com/", "https://github.com/", 1),
    };
    let repository = repository.replacen("https://www.github.com/", "https://github.com/", 1);
    let Some(path) = repository.strip_prefix("https://github.com/") else {
        return repository.trim_end_matches('/').to_string();
    };
    let mut parts = path.split('/').filter(|part| !part.is_empty());
    match (parts.next(), parts.next()) {
        (Some(owner), Some(name)) => {
            let name = name.strip_suffix(".git").unwrap_or(name);
            format!("https://github.com/{owner}/{name}")
        }
        _ => repository.trim_end_matches('/').to_string(),
    }
}

/// `https://github.com/owner/repo`, as [`normalize_repository`] leaves it.
fn github_repository(repository: &str) -> bool {
    repository
        .strip_prefix("https://github.com/")
        .is_some_and(|path| path.split('/').count() == 2)
}

/// A new issue of `repository` on the form `template`, its fields filled by `params` (and `title`),
/// within [`URL_BUDGET`]: the crash's report is cut first, then the plugins, then the rest.
fn issue_url(repository: &str, template: &str, mut params: Vec<(&str, String)>) -> String {
    params.insert(0, ("template", template.to_string()));
    let base = format!("{repository}/issues/new");
    let mut url = with_query(&base, &params);
    for name in [field::LOGS, field::PLUGINS, field::WHAT_HAPPENED] {
        if url.len() <= URL_BUDGET {
            break;
        }
        let excess = url.len() - URL_BUDGET;
        if let Some((_, value)) = params.iter_mut().find(|(key, _)| *key == name) {
            let room = encode(value).len().saturating_sub(excess);
            *value = cut(value, room);
            url = with_query(&base, &params);
        }
    }
    url
}

fn with_query(base: &str, params: &[(&str, String)]) -> String {
    let mut url = base.to_string();
    for (index, (key, value)) in params.iter().enumerate() {
        url.push(if index == 0 { '?' } else { '&' });
        url.push_str(&encode(key));
        url.push('=');
        url.push_str(&encode(value));
    }
    url
}

/// The longest start of `text` that, with [`CUT_MARK`] after it, takes at most `room` characters
/// when encoded; it ends at a line's end when one is near. All of `text` when it fits.
fn cut(text: &str, room: usize) -> String {
    if encode(text).len() <= room {
        return text.to_string();
    }
    let room = room.saturating_sub(encode(CUT_MARK).len());
    let mut taken = 0;
    let mut end = 0;
    for (index, ch) in text.char_indices() {
        let mut buffer = [0; 4];
        taken += encode(ch.encode_utf8(&mut buffer)).len();
        if taken > room {
            break;
        }
        end = index + ch.len_utf8();
    }
    let head = &text[..end];
    // Whole lines read better than a line cut in the middle, unless that loses a lot.
    let head = match head.rfind('\n') {
        Some(line_end) if line_end >= head.len() / 2 => &head[..line_end],
        _ => head,
    };
    format!("{head}{CUT_MARK}")
}

/// Percent-encodes a query's key or value: everything but the unreserved characters of RFC 3986,
/// UTF-8 byte by byte.
fn encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

pub fn init(cx: &mut App) {
    cx.on_action(|_: &ReportBug, cx| report_bug(None, cx));
    cx.on_action(|_: &SuggestFeature, cx| open_url(&idea_url(&machine()), cx));
    cx.on_action(|_: &AskQuestion, cx| open_url(&question_url(&machine()), cx));
}

/// The Mac's facts, from `sysctl`.
#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::CString;

    /// "macOS 26.1 (25B78)"; "macOS" when the system doesn't say.
    pub fn system() -> String {
        let version = sysctl_text("kern.osproductversion");
        let build = sysctl_text("kern.osversion");
        match (version, build) {
            (Some(version), Some(build)) => format!("macOS {version} ({build})"),
            (Some(version), None) => format!("macOS {version}"),
            _ => "macOS".to_string(),
        }
    }

    /// "Apple M3 Pro, arm64"; a Flux for Intel under Rosetta — "…, x86_64 (Rosetta)".
    pub fn processor() -> String {
        let mut parts = Vec::new();
        parts.extend(sysctl_text("machdep.cpu.brand_string"));
        let mut arch = arch().to_string();
        if sysctl_number("sysctl.proc_translated") == Some(1) {
            arch.push_str(" (Rosetta)");
        }
        parts.push(arch);
        parts.join(", ")
    }

    /// The memory in whole gigabytes.
    pub fn memory_gb() -> Option<u64> {
        let bytes = sysctl_number("hw.memsize").filter(|bytes| *bytes > 0)?;
        Some(bytes.div_ceil(1 << 30))
    }

    /// The architecture Flux is built for, as macOS names it: `arm64` (Rust's `aarch64`), `x86_64`.
    pub fn arch() -> &'static str {
        match std::env::consts::ARCH {
            "aarch64" => "arm64",
            other => other,
        }
    }

    /// A string value.
    fn sysctl_text(name: &str) -> Option<String> {
        let bytes = sysctl(name)?;
        let text = String::from_utf8_lossy(&bytes);
        let text = text.trim_end_matches('\0').trim();
        (!text.is_empty()).then(|| text.to_string())
    }

    /// A 32- or 64-bit integer value.
    fn sysctl_number(name: &str) -> Option<u64> {
        let bytes = sysctl(name)?;
        match bytes.len() {
            4 => Some(u32::from_ne_bytes(bytes.try_into().ok()?) as u64),
            8 => Some(u64::from_ne_bytes(bytes.try_into().ok()?)),
            _ => None,
        }
    }

    fn sysctl(name: &str) -> Option<Vec<u8>> {
        let name = CString::new(name).ok()?;
        let mut size: libc::size_t = 0;
        // SAFETY: a NUL-terminated name; a null buffer asks only for the value's size.
        let asked = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                std::ptr::null_mut(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if asked != 0 || size == 0 {
            return None;
        }
        let mut buffer = vec![0u8; size];
        // SAFETY: the buffer holds `size` bytes, as the size says; nothing is written to the
        // system (a null new value).
        let read = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                buffer.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if read != 0 {
            return None;
        }
        buffer.truncate(size);
        Some(buffer)
    }
}

/// Other systems: what Rust knows about them.
#[cfg(not(target_os = "macos"))]
mod mac {
    pub fn system() -> String {
        std::env::consts::OS.to_string()
    }

    pub fn processor() -> String {
        std::env::consts::ARCH.to_string()
    }

    pub fn memory_gb() -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> SystemInfo {
        SystemInfo {
            version: "0.1.0 (ca086c2)".into(),
            macos: "macOS 26.1 (25B78)".into(),
            hardware: "Apple M3 Pro, arm64, 36 GB".into(),
            language: "English".into(),
            plugins: vec![
                ("flux.rust".into(), "0.1.0".into()),
                ("flux.yandex".into(), "0.1.0, development".into()),
            ],
        }
    }

    /// The value of a query parameter, decoded.
    fn param(url: &str, key: &str) -> Option<String> {
        let query = url.split_once('?')?.1;
        query.split('&').find_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            (name == key).then(|| decode(value))
        })
    }

    fn decode(text: &str) -> String {
        let bytes = text.as_bytes();
        let mut out = Vec::new();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'%' {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap();
                out.push(u8::from_str_radix(hex, 16).unwrap());
                index += 3;
            } else {
                out.push(bytes[index]);
                index += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn encoding_keeps_only_unreserved_characters() {
        assert_eq!(encode("aZ09-_.~"), "aZ09-_.~");
        assert_eq!(
            encode("a b\n&=?#+/%"),
            "a%20b%0A%26%3D%3F%23%2B%2F%25"
        );
        assert_eq!(encode("€ Ж"), "%E2%82%AC%20%D0%96");
        assert_eq!(decode(&encode("Привет, мир & co\n")), "Привет, мир & co\n");
    }

    #[test]
    fn the_bug_form_gets_the_facts() {
        let url = bug_report_url(&info(), None);
        assert!(url.starts_with("https://github.com/egor4geev/flux/issues/new?template=bug_report.yml&"));
        assert_eq!(param(&url, "version").as_deref(), Some("0.1.0 (ca086c2)"));
        assert_eq!(param(&url, "macos").as_deref(), Some("macOS 26.1 (25B78)"));
        assert_eq!(param(&url, "hardware").as_deref(), Some("Apple M3 Pro, arm64, 36 GB"));
        assert_eq!(
            param(&url, "plugins").as_deref(),
            Some("flux.rust 0.1.0\nflux.yandex 0.1.0, development")
        );
        assert_eq!(param(&url, "title"), None);
        assert_eq!(param(&url, "logs"), None);
        let none = SystemInfo {
            plugins: Vec::new(),
            ..info()
        };
        assert_eq!(
            param(&bug_report_url(&none, None), "plugins").as_deref(),
            Some("No plugins besides the built-in ones")
        );
    }

    fn crash(backtrace: String) -> Crash {
        Crash {
            message: "index out of bounds: the len is 3 but the index is 7".into(),
            location: "crates/flux-app/src/editor.rs:120:9".into(),
            thread: "main".into(),
            backtrace,
            version: "0.1.0 (1234567)".into(),
            time: "2026-10-15 14:03:12".into(),
            file: Default::default(),
        }
    }

    #[test]
    fn a_crash_fills_the_title_the_story_and_the_log() {
        let url = bug_report_url(&info(), Some(&crash("   0: flux::main\n   1: start".into())));
        assert_eq!(
            param(&url, "title").as_deref(),
            Some("Crash: index out of bounds: the len is 3 but the index is 7")
        );
        assert_eq!(
            param(&url, "what-happened").as_deref(),
            Some("Flux crashed: index out of bounds: the len is 3 but the index is 7")
        );
        assert_eq!(
            param(&url, "logs").as_deref(),
            Some(
                "2026-10-15 14:03:12\nthread 'main' panicked at crates/flux-app/src/editor.rs:120:9:\n\
                 index out of bounds: the len is 3 but the index is 7\n\n   0: flux::main\n   1: start"
            )
        );
        // The version that crashed, not this one.
        assert_eq!(param(&url, "version").as_deref(), Some("0.1.0 (1234567)"));
    }

    #[test]
    fn a_long_title_is_shortened() {
        let mut long = crash(String::new());
        long.message = format!("{}\nsecond line", "x".repeat(100));
        let title = crash_title(&long);
        assert_eq!(title, format!("Crash: {}…", "x".repeat(TITLE_CHARS)));
    }

    #[test]
    fn a_long_stack_is_cut_to_the_budget_from_its_end() {
        let frames: Vec<String> = (0..2000)
            .map(|frame| format!("  {frame}: flux_app::editor::Editor::frame_{frame} — ж"))
            .collect();
        let url = bug_report_url(&info(), Some(&crash(frames.join("\n"))));
        assert!(url.len() <= URL_BUDGET, "{} > {URL_BUDGET}", url.len());
        let logs = param(&url, "logs").unwrap();
        assert!(logs.contains("thread 'main' panicked"));
        assert!(logs.contains("  0: flux_app::editor::Editor::frame_0"));
        assert!(!logs.contains("frame_1999"));
        // Cut at a line's end, marked.
        assert!(logs.ends_with("— ж\n…"), "{}", &logs[logs.len() - 40..]);
        // The rest is whole.
        assert_eq!(param(&url, "version").as_deref(), Some("0.1.0 (1234567)"));
        assert!(param(&url, "plugins").unwrap().ends_with("development"));
    }

    #[test]
    fn many_plugins_are_cut_too() {
        let many = SystemInfo {
            plugins: (0..1000)
                .map(|n| (format!("someone.plugin-{n}"), "1.0.0".to_string()))
                .collect(),
            ..info()
        };
        let url = bug_report_url(&many, None);
        assert!(url.len() <= URL_BUDGET);
        let plugins = param(&url, "plugins").unwrap();
        assert!(plugins.starts_with("someone.plugin-0 1.0.0\n"));
        assert!(plugins.ends_with("\n…"));
    }

    #[test]
    fn the_text_lists_the_facts() {
        assert_eq!(
            info().to_text(),
            "Flux 0.1.0 (ca086c2)\nmacOS 26.1 (25B78)\nApple M3 Pro, arm64, 36 GB\n\
             UI language: English\nPlugins:\n    flux.rust 0.1.0\n    flux.yandex 0.1.0, development"
        );
        let bare = SystemInfo {
            version: "0.1.0".into(),
            macos: "macOS 26.1 (25B78)".into(),
            hardware: "Apple M3 Pro, arm64".into(),
            ..SystemInfo::default()
        };
        assert_eq!(bare.to_text(), "Flux 0.1.0\nmacOS 26.1 (25B78)\nApple M3 Pro, arm64");
    }

    fn plugin(repository: Option<&str>, bundled: bool) -> PluginReport {
        PluginReport {
            id: "flux.yandex".into(),
            name: "Yandex Tracker & Calendar".into(),
            version: "0.1.0".into(),
            repository: repository.map(str::to_string),
            bundled,
        }
    }

    #[test]
    fn a_catalog_plugin_goes_to_the_catalog() {
        for repository in [
            "https://github.com/egor4geev/flux-plugins",
            "https://github.com/egor4geev/flux-plugins/",
            "https://github.com/egor4geev/flux-plugins.git",
            "https://github.com/egor4geev/flux-plugins/tree/main/plugins/flux.yandex",
            "git@github.com:egor4geev/flux-plugins.git",
        ] {
            let url = plugin_issue_url(&plugin(Some(repository), false), &info()).unwrap();
            assert!(
                url.starts_with(
                    "https://github.com/egor4geev/flux-plugins/issues/new?template=plugin_bug.yml&"
                ),
                "{repository}: {url}"
            );
            assert_eq!(
                param(&url, "plugin").as_deref(),
                Some("Yandex Tracker & Calendar (flux.yandex)")
            );
            assert_eq!(param(&url, "plugin-version").as_deref(), Some("0.1.0"));
            assert_eq!(param(&url, "version").as_deref(), Some("0.1.0 (ca086c2)"));
            assert_eq!(param(&url, "macos").as_deref(), Some("macOS 26.1 (25B78)"));
            assert_eq!(param(&url, "hardware").as_deref(), Some("Apple M3 Pro, arm64, 36 GB"));
        }
    }

    #[test]
    fn a_built_in_plugin_is_flux() {
        for report in [
            plugin(Some("https://github.com/egor4geev/flux"), true),
            plugin(None, true),
            plugin(Some("https://github.com/egor4geev/flux.git"), false),
        ] {
            let url = plugin_issue_url(&report, &info()).unwrap();
            assert!(url.starts_with(
                "https://github.com/egor4geev/flux/issues/new?template=bug_report.yml&"
            ));
            assert_eq!(
                param(&url, "what-happened").as_deref(),
                Some("The plugin Yandex Tracker & Calendar (flux.yandex) 0.1.0:\n\n")
            );
            assert_eq!(param(&url, "version").as_deref(), Some("0.1.0 (ca086c2)"));
        }
    }

    #[test]
    fn other_plugins_go_to_their_repository() {
        let url = |repository: &str| plugin_issue_url(&plugin(Some(repository), false), &info());
        assert_eq!(
            url("https://github.com/someone/flux-hello/").as_deref(),
            Some("https://github.com/someone/flux-hello/issues/new")
        );
        assert_eq!(
            url("https://github.com/someone/plugins/tree/main/hello").as_deref(),
            Some("https://github.com/someone/plugins/issues/new")
        );
        assert_eq!(
            url("https://gitlab.com/someone/hello").as_deref(),
            Some("https://gitlab.com/someone/hello")
        );
        assert_eq!(url("not a link"), None);
        assert_eq!(plugin_issue_url(&plugin(None, false), &info()), None);
        assert!(!plugin(None, false).reportable());
        assert!(plugin(None, true).reportable());
    }

    #[test]
    fn discussions_get_their_category_and_the_version() {
        assert_eq!(
            idea_url(&info()),
            "https://github.com/egor4geev/flux/discussions/new?category=ideas\
             &version=0.1.0%20%28ca086c2%29"
        );
        assert_eq!(
            question_url(&info()),
            "https://github.com/egor4geev/flux/discussions/new?category=q-a\
             &version=0.1.0%20%28ca086c2%29&macos=macOS%2026.1%20%2825B78%29"
        );
    }

    #[test]
    fn this_mac_is_described() {
        let machine = machine();
        assert!(machine.version.starts_with(env!("CARGO_PKG_VERSION")));
        assert!(!machine.hardware.is_empty());
        #[cfg(target_os = "macos")]
        {
            assert!(machine.macos.starts_with("macOS "), "{}", machine.macos);
            assert!(machine.hardware.contains(mac::arch()), "{}", machine.hardware);
            assert!(!machine.hardware.contains("aarch64"), "{}", machine.hardware);
            assert!(machine.hardware.ends_with(" GB"), "{}", machine.hardware);
        }
    }
}
