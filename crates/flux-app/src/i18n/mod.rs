//! Localization: the interface speaks English or Russian.
//!
//! Strings stay English in the code; [`tr`] returns their translation for the current
//! language and falls back to English when a translation is missing. The language is chosen
//! at startup ([`init`]): `FLUX_LANG=en|ru` if set, otherwise the first supported language
//! among the system's preferred languages, otherwise English. Since stage 8.3 Settings →
//! Appearance → Language chooses it too ([`apply_choice`]), while Flux runs: what is drawn next
//! speaks the new language.
//!
//! - `tr("Search files")` — a plain string;
//! - `trf("{0} of {1} files", &[&shown, &total])` — a template with numbered arguments;
//! - `trn(count, "{n} result", "{n} results")` — a count with plural forms (Russian has three).
//!
//! Russian translations live in `i18n/ru/*.rs`, one table per area of the UI. The strings name
//! macOS's places (the Finder, the Trash); on Windows [`tr`] takes Windows' words first
//! ([`WINDOWS_WORDS`]) and translates those.

mod ru;

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::keymap::KeyOs;
use crate::settings::LanguageChoice;

/// A language of the interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    Ru,
}

/// The language of the interface: 0 — English, 1 — Russian.
static LANG: AtomicU8 = AtomicU8::new(0);

fn set_lang(lang: Lang) {
    LANG.store(
        match lang {
            Lang::En => 0,
            Lang::Ru => 1,
        },
        Ordering::Relaxed,
    );
}

/// Chooses the language of the interface by `FLUX_LANG` and the system's languages; call before
/// any window opens.
pub fn init() {
    let override_lang = std::env::var("FLUX_LANG").ok();
    set_lang(choose(override_lang.as_deref(), &system_languages()));
}

/// The language Settings → Appearance → Language chose; `FLUX_LANG` still wins. Windows draw the
/// new language from their next frame (`cx.refresh_windows()`; the app menu is built anew by
/// `app_menu::rebuild`).
pub fn apply_choice(choice: LanguageChoice) {
    let override_lang = std::env::var("FLUX_LANG").ok();
    set_lang(resolve(
        override_lang.as_deref(),
        choice,
        &system_languages(),
    ));
}

/// The language of a choice: `FLUX_LANG` (`override_lang`) wins, then the choice; "System" is the
/// first of the system's preferred languages Flux speaks, otherwise English.
fn resolve(override_lang: Option<&str>, choice: LanguageChoice, preferred: &[String]) -> Lang {
    match (override_lang.and_then(parse), choice) {
        (Some(lang), _) => lang,
        (None, LanguageChoice::English) => Lang::En,
        (None, LanguageChoice::Russian) => Lang::Ru,
        (None, LanguageChoice::System) => choose(None, preferred),
    }
}

/// The language "System" stands for now: the first of the system's preferred languages Flux
/// speaks, otherwise English.
pub fn system_lang() -> Lang {
    choose(None, &system_languages())
}

/// `FLUX_LANG`, when it names a language Flux speaks: it decides the language for the whole run.
pub fn override_lang() -> Option<(String, Lang)> {
    let value = std::env::var("FLUX_LANG").ok()?;
    let lang = parse(&value)?;
    Some((value, lang))
}

/// A language's own name, as its speakers write it: the choices of Settings → Appearance →
/// Language are never translated.
pub fn lang_name(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "English",
        Lang::Ru => "Русский",
    }
}

/// The language of the interface (English until [`init`]).
pub fn lang() -> Lang {
    match LANG.load(Ordering::Relaxed) {
        1 => Lang::Ru,
        _ => Lang::En,
    }
}

/// The interface language as a code: "en", "ru" (plugins' translations are keyed by it).
pub fn lang_code() -> &'static str {
    match lang() {
        Lang::En => "en",
        Lang::Ru => "ru",
    }
}

/// `FLUX_LANG` wins; otherwise the first preferred language we speak; otherwise English.
fn choose(override_lang: Option<&str>, preferred: &[String]) -> Lang {
    override_lang
        .and_then(parse)
        .or_else(|| preferred.iter().find_map(|tag| parse(tag)))
        .unwrap_or(Lang::En)
}

/// `ru`, `ru-RU`, `ru_RU` → Russian; `en…` → English; anything else — not supported.
fn parse(tag: &str) -> Option<Lang> {
    let primary = tag.split(['-', '_']).next()?.to_ascii_lowercase();
    match primary.as_str() {
        "ru" => Some(Lang::Ru),
        "en" => Some(Lang::En),
        _ => None,
    }
}

/// Preferred languages of the user, most preferred first (System Settings → Language & Region).
#[cfg(target_os = "macos")]
pub(crate) fn system_languages() -> Vec<String> {
    use core_foundation::array::CFArray;
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use core_foundation_sys::locale::CFLocaleCopyPreferredLanguages;

    // SAFETY: the function follows the Create rule and returns an array of CFStrings.
    let languages: CFArray<CFString> =
        unsafe { CFArray::wrap_under_create_rule(CFLocaleCopyPreferredLanguages()) };
    languages
        .iter()
        .map(|language| language.to_string())
        .collect()
}

/// The display languages of Windows, most preferred first (Settings → Time & language → Language
/// & region): "ru-RU", "en-US".
#[cfg(windows)]
pub(crate) fn system_languages() -> Vec<String> {
    use windows_sys::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};
    let (mut count, mut len) = (0u32, 0u32);
    // SAFETY: a null buffer asks for the length; the second call writes at most `len` characters
    // into the buffer, which outlives it.
    unsafe {
        let asked = GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            std::ptr::null_mut(),
            &mut len,
        );
        if asked == 0 || len == 0 {
            return Vec::new();
        }
        let mut buffer = vec![0u16; len as usize];
        let read = GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            buffer.as_mut_ptr(),
            &mut len,
        );
        if read == 0 {
            return Vec::new();
        }
        buffer.truncate(len as usize);
        multi_string(&buffer)
    }
}

/// The strings of a Windows multi-string: each ends with a NUL, an empty one ends the list.
#[cfg(any(windows, test))]
fn multi_string(buffer: &[u16]) -> Vec<String> {
    buffer
        .split(|&c| c == 0)
        .take_while(|part| !part.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) fn system_languages() -> Vec<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .collect()
}

/// The translation of `text` into the interface language; `text` itself when there is none. On
/// Windows, Windows' words for the system's places come first ([`for_system`]).
pub fn tr(text: &str) -> &str {
    translate(lang(), for_system(KeyOs::current(), text))
}

/// `text` as the system words it: the strings are written for macOS; on Windows the ones that name
/// macOS's places become [`WINDOWS_WORDS`]'.
fn for_system(os: KeyOs, text: &str) -> &str {
    match os {
        KeyOs::Mac => text,
        KeyOs::Windows => windows_words().get(text).copied().unwrap_or(text),
    }
}

fn windows_words() -> &'static HashMap<&'static str, &'static str> {
    static WORDS: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    WORDS.get_or_init(|| WINDOWS_WORDS.iter().copied().collect())
}

/// The strings that name macOS's places, as Windows words them: Explorer for the Finder, the
/// Recycle Bin for the Trash, "this computer" for "this Mac". Each has its Russian translation.
const WINDOWS_WORDS: &[(&str, &str)] = &[
    ("Finder", "Explorer"),
    ("Reveal in Finder", "Reveal in Explorer"),
    ("Reveal In Finder", "Reveal In Explorer"),
    ("Show in Finder", "Show in Explorer"),
    ("Show Log in Finder", "Show Log in Explorer"),
    (
        "Flux started from the Dock or the Finder opens the projects that were open when it quit.",
        "Flux started from the Start menu or the taskbar opens the projects that were open when it quit.",
    ),
    ("Move to Trash", "Move to Recycle Bin"),
    ("Move To Trash", "Move To Recycle Bin"),
    ("Move “{0}” to Trash?", "Move “{0}” to the Recycle Bin?"),
    ("Move {0} to Trash?", "Move {0} to the Recycle Bin?"),
    (
        "The folder and everything in it will be moved to the Trash.",
        "The folder and everything in it will be moved to the Recycle Bin.",
    ),
    (
        "You can restore it from the Trash.",
        "You can restore it from the Recycle Bin.",
    ),
    ("Couldn't move to Trash", "Couldn't move to the Recycle Bin"),
    (
        "Files Claude created are moved to the Trash.",
        "Files Claude created are moved to the Recycle Bin.",
    ),
    (
        "Its transcript goes to the Trash.",
        "Its transcript goes to the Recycle Bin.",
    ),
    (
        "It closes, and its transcript goes to the Trash.",
        "It closes, and its transcript goes to the Recycle Bin.",
    ),
    ("to the Trash", "to the Recycle Bin"),
    ("While macOS is dark", "While Windows is in dark mode"),
    ("While macOS is light", "While Windows is in light mode"),
    (
        "Switch between a light and a dark theme together with macOS.",
        "Switch between a light and a dark theme together with Windows.",
    ),
    (
        "macOS is dark now: Flux shows the dark theme.",
        "Windows is in dark mode now: Flux shows the dark theme.",
    ),
    (
        "macOS is light now: Flux shows the light theme.",
        "Windows is in light mode now: Flux shows the light theme.",
    ),
    (
        "Reduce motion is on in macOS’s Accessibility settings: everything simply appears.",
        "Animation effects are off in the Accessibility settings of Windows: everything simply appears.",
    ),
    (
        "A macOS notification while Flux isn't the active app.",
        "A Windows notification while Flux isn't the active app.",
    ),
    (
        "The version, macOS, the computer and the plugins — for a report",
        "The version, Windows, the computer and the plugins — for a report",
    ),
    (
        "A fast native code editor for macOS — with code understanding, version control and Claude Code built in.",
        "A fast native code editor for Windows — with code understanding, version control and Claude Code built in.",
    ),
    (
        "Asking a neutral service where the network sees this Mac",
        "Asking a neutral service where the network sees this computer",
    ),
    (
        "Flux finds out which country the network shows for this Mac and stops Claude while that country is on the list below or can't be found out.",
        "Flux finds out which country the network shows for this computer and stops Claude while that country is on the list below or can't be found out.",
    ),
    (
        "Accept connections from programs on this Mac",
        "Accept connections from programs on this computer",
    ),
    ("uses the one on this Mac", "uses the one on this computer"),
    (
        "The languages found on this Mac are checked. Flux installs their plugins from the catalog; a language server comes by itself when you open the first file.",
        "The languages found on this computer are checked. Flux installs their plugins from the catalog; a language server comes by itself when you open the first file.",
    ),
    ("Found on this Mac", "Found on this computer"),
    (
        "Flux starts the server for the language of an open file: errors, completion, go to definition. Servers installed on this Mac are used as they are; those Flux installed can be updated or deleted here.",
        "Flux starts the server for the language of an open file: errors, completion, go to definition. Servers installed on this computer are used as they are; those Flux installed can be updated or deleted here.",
    ),
    (
        "When a file needs a server that isn't on this Mac, Flux downloads it to:",
        "When a file needs a server that isn't on this computer, Flux downloads it to:",
    ),
    ("On this Mac · {0}", "On this computer · {0}"),
];

fn translate(lang: Lang, text: &str) -> &str {
    match lang {
        Lang::En => text,
        Lang::Ru => ru_strings().get(text).copied().unwrap_or(text),
    }
}

/// [`tr`] of a template, then `{0}`, `{1}`, … replaced with `args`.
pub fn trf(template: &str, args: &[&dyn Display]) -> String {
    fill(tr(template), args)
}

/// One pass over the template: an argument that itself contains `{1}` (`stash@{1}`) is inserted
/// as it is, not filled in by a later placeholder.
fn fill(template: &str, args: &[&dyn Display]) -> String {
    let mut text = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        text.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let placeholder = after.find('}').and_then(|close| {
            let index: usize = after[..close].parse().ok()?;
            Some((index, close))
        });
        match placeholder {
            Some((index, close)) if index < args.len() => {
                text.push_str(&args[index].to_string());
                rest = &after[close + 1..];
            }
            _ => {
                text.push('{');
                rest = after;
            }
        }
    }
    text.push_str(rest);
    text
}

/// A count with the right plural form: `trn(3, "{n} result", "{n} results")` → "3 results", "3
/// результата". `{n}` is replaced with the count.
pub fn trn(count: usize, one: &str, other: &str) -> String {
    plural(lang(), count, one, other).replace("{n}", &count.to_string())
}

fn plural<'a>(lang: Lang, count: usize, one: &'a str, other: &'a str) -> &'a str {
    match lang {
        Lang::En if count == 1 => one,
        Lang::En => other,
        Lang::Ru => match ru_plurals().get(one) {
            Some(forms) => forms[ru_plural_form(count)],
            None if count == 1 => one,
            None => other,
        },
    }
}

/// Russian plural form: 0 — "1 файл", 1 — "2 файла", 2 — "5 файлов".
fn ru_plural_form(count: usize) -> usize {
    let (last, last_two) = (count % 10, count % 100);
    if last == 1 && last_two != 11 {
        0
    } else if (2..=4).contains(&last) && !(12..=14).contains(&last_two) {
        1
    } else {
        2
    }
}

fn ru_strings() -> &'static HashMap<&'static str, &'static str> {
    static STRINGS: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    STRINGS.get_or_init(|| {
        ru::STRINGS
            .iter()
            .flat_map(|table| table.iter().copied())
            .collect()
    })
}

fn ru_plurals() -> &'static HashMap<&'static str, [&'static str; 3]> {
    static PLURALS: OnceLock<HashMap<&'static str, [&'static str; 3]>> = OnceLock::new();
    PLURALS.get_or_init(|| {
        ru::PLURALS
            .iter()
            .flat_map(|table| table.iter().copied())
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_comes_from_override_then_system() {
        let system = |tags: &[&str]| tags.iter().map(|t| t.to_string()).collect::<Vec<_>>();
        assert_eq!(choose(None, &system(&["ru-RU", "en-US"])), Lang::Ru);
        assert_eq!(choose(None, &system(&["en-GB", "ru-RU"])), Lang::En);
        // Unsupported languages are skipped in order of preference.
        assert_eq!(choose(None, &system(&["de-DE", "ru-RU"])), Lang::Ru);
        assert_eq!(choose(None, &system(&["de-DE"])), Lang::En);
        assert_eq!(choose(None, &[]), Lang::En);
        assert_eq!(choose(Some("en"), &system(&["ru-RU"])), Lang::En);
        assert_eq!(choose(Some("ru_RU.UTF-8"), &system(&["en-US"])), Lang::Ru);
        assert_eq!(choose(Some("xx"), &system(&["ru"])), Lang::Ru);
    }

    #[test]
    fn a_choice_gives_its_language_unless_flux_lang_says_otherwise() {
        let system = |tags: &[&str]| tags.iter().map(|t| t.to_string()).collect::<Vec<_>>();
        let russian_mac = system(&["ru-RU", "en-US"]);
        assert_eq!(
            resolve(None, LanguageChoice::System, &russian_mac),
            Lang::Ru
        );
        assert_eq!(
            resolve(None, LanguageChoice::English, &russian_mac),
            Lang::En
        );
        assert_eq!(
            resolve(None, LanguageChoice::Russian, &system(&["en-US"])),
            Lang::Ru
        );
        assert_eq!(
            resolve(None, LanguageChoice::System, &system(&["de"])),
            Lang::En
        );
        // FLUX_LANG wins over the choice; a value Flux doesn't speak is ignored.
        assert_eq!(
            resolve(Some("en"), LanguageChoice::Russian, &russian_mac),
            Lang::En
        );
        assert_eq!(
            resolve(Some("xx"), LanguageChoice::English, &russian_mac),
            Lang::En
        );
        assert_eq!(lang_name(Lang::Ru), "Русский");
    }

    #[test]
    fn russian_plural_forms() {
        let forms = |n| ru_plural_form(n);
        assert_eq!([1, 21, 101, 1001].map(forms), [0; 4]);
        assert_eq!([2, 3, 4, 22, 34, 102].map(forms), [1; 6]);
        assert_eq!([0, 5, 11, 12, 14, 15, 25, 111, 112].map(forms), [2; 9]);
    }

    #[test]
    fn templates_are_filled_by_position() {
        assert_eq!(fill("{0} of {1} files", &[&3, &12]), "3 of 12 files");
        assert_eq!(fill("{1} / {0}", &[&"a", &"b"]), "b / a");
        // An argument with braces in it stays as it is.
        assert_eq!(
            fill("Applied {0}: {1}", &[&"stash@{1}", &"WIP"]),
            "Applied stash@{1}: WIP"
        );
        assert_eq!(fill("{x} {9} {0}", &[&"a"]), "{x} {9} a");
    }

    /// Windows' words replace macOS's in both languages: every replaced string is one the sources
    /// use (it has a Russian translation), and its Windows wording has one too, free of macOS's
    /// names.
    #[test]
    fn windows_words_its_places_in_both_languages() {
        for (mac, windows) in WINDOWS_WORDS {
            assert!(
                ru_strings().contains_key(mac),
                "not a source string: {mac:?}"
            );
            let russian = ru_strings().get(windows).copied();
            assert!(russian.is_some(), "no Russian for {windows:?}");
            for name in ["Finder", "Trash", "macOS", "Mac", "Dock"] {
                assert!(!windows.contains(name), "{windows:?}");
                assert!(!russian.unwrap_or_default().contains(name), "{russian:?}");
            }
        }
        assert_eq!(
            for_system(KeyOs::Windows, "Reveal in Finder"),
            "Reveal in Explorer"
        );
        assert_eq!(
            translate(Lang::Ru, for_system(KeyOs::Windows, "Reveal in Finder")),
            "Показать в Проводнике"
        );
        assert_eq!(
            for_system(KeyOs::Mac, "Reveal in Finder"),
            "Reveal in Finder"
        );
        assert_eq!(for_system(KeyOs::Windows, "Search files"), "Search files");
    }

    #[test]
    fn windows_languages_come_as_a_multi_string() {
        let buffer: Vec<u16> = "ru-RU\0en-US\0\0".encode_utf16().collect();
        assert_eq!(multi_string(&buffer), ["ru-RU", "en-US"]);
        assert!(multi_string(&[0, 0]).is_empty());
        assert!(multi_string(&[]).is_empty());
    }

    #[test]
    fn missing_translations_fall_back_to_english() {
        assert_eq!(
            translate(Lang::Ru, "No such string in any table"),
            "No such string in any table"
        );
        assert_eq!(translate(Lang::En, "Search files"), "Search files");
        assert_eq!(plural(Lang::En, 1, "{n} file", "{n} files"), "{n} file");
        assert_eq!(plural(Lang::En, 2, "{n} file", "{n} files"), "{n} files");
    }

    /// Every table entry is non-empty, no string is translated twice in different ways, and
    /// every `tr("…")` literal in the sources has a Russian translation.
    #[test]
    fn russian_table_covers_the_sources() {
        let mut seen: HashMap<&str, &str> = HashMap::new();
        for (en, ru) in ru::STRINGS.iter().flat_map(|table| table.iter()) {
            assert!(!en.is_empty() && !ru.is_empty(), "empty entry: {en:?}");
            if let Some(previous) = seen.insert(en, ru) {
                assert_eq!(previous, *ru, "conflicting translations of {en:?}");
            }
        }
        let plurals: HashMap<&str, [&str; 3]> = ru::PLURALS
            .iter()
            .flat_map(|table| table.iter().copied())
            .collect();
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut missing = Vec::new();
        // The localization module itself only holds examples and the scanner.
        for path in source_files(&src)
            .into_iter()
            .filter(|p| !p.starts_with(src.join("i18n")))
        {
            let source = std::fs::read_to_string(&path).unwrap();
            for (call, literal) in literals(&source) {
                let known = match call {
                    "trn" => plurals.contains_key(literal),
                    _ => seen.contains_key(literal),
                };
                if !known {
                    missing.push(format!("{}: {call}({literal:?})", path.display()));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "missing Russian:\n{}",
            missing.join("\n")
        );
    }

    fn source_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.extend(source_files(&path));
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
        files
    }

    /// First string literal of every `tr(`, `trf(` and `trn(` call (for `trn` — the one
    /// after the count).
    fn literals(source: &str) -> Vec<(&'static str, &str)> {
        let mut found = Vec::new();
        for call in ["tr(\"", "trf(\"", "trn("] {
            let name = &call[..call.find('(').unwrap()];
            let name: &'static str = match name {
                "tr" => "tr",
                "trf" => "trf",
                _ => "trn",
            };
            let mut rest = source;
            while let Some(at) = rest.find(call) {
                // Skip `fn tr(`, `str(` and the like: the call must not continue an identifier.
                let before = rest[..at].chars().next_back();
                rest = &rest[at + call.len()..];
                if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                let literal_start = if name == "trn" {
                    match rest.find('"') {
                        Some(quote) if !rest[..quote].contains(')') => quote + 1,
                        _ => continue,
                    }
                } else {
                    0
                };
                let body = &rest[literal_start..];
                if let Some(end) = body.find('"') {
                    found.push((name, &body[..end]));
                }
            }
        }
        found
    }
}
