//! Localization: the interface speaks English or Russian.
//!
//! Strings stay English in the code; [`tr`] returns their translation for the current
//! language and falls back to English when a translation is missing. The language is chosen
//! once at startup ([`init`]): `FLUX_LANG=en|ru` if set, otherwise the first supported
//! language among the system's preferred languages, otherwise English.
//!
//! - `tr("Search files")` — a plain string;
//! - `trf("{0} of {1} files", &[&shown, &total])` — a template with numbered arguments;
//! - `trn(count, "{n} result", "{n} results")` — a count with plural forms (Russian has three).
//!
//! Russian translations live in `i18n/ru/*.rs`, one table per area of the UI.

mod ru;

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;

/// A language of the interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    Ru,
}

static LANG: OnceLock<Lang> = OnceLock::new();

/// Chooses the language of the interface; call once before any window opens.
pub fn init() {
    let override_lang = std::env::var("FLUX_LANG").ok();
    LANG.set(choose(override_lang.as_deref(), &system_languages()))
        .ok();
}

/// The language of the interface (English until [`init`]).
pub fn lang() -> Lang {
    LANG.get().copied().unwrap_or(Lang::En)
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
fn system_languages() -> Vec<String> {
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

#[cfg(not(target_os = "macos"))]
fn system_languages() -> Vec<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .collect()
}

/// The translation of `text` into the interface language; `text` itself when there is none.
pub fn tr(text: &str) -> &str {
    translate(lang(), text)
}

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
