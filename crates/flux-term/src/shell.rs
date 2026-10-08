//! The program a terminal runs and its environment.

use std::collections::HashMap;
use std::env;
use std::path::Path;
use std::sync::OnceLock;

/// The user's shell as a login shell: `$SHELL`, otherwise the account's shell from the user
/// database, otherwise `/bin/zsh` (the macOS default). A login shell reads the profile, so `PATH`
/// is complete even when Flux was started from Finder with a minimal environment.
pub(crate) fn login_shell() -> (String, Vec<String>) {
    let shell = env::var("SHELL")
        .ok()
        .filter(|shell| !shell.is_empty())
        .or_else(account_shell)
        .unwrap_or_else(|| "/bin/zsh".to_string());
    (shell, vec!["-l".to_string()])
}

/// The login shell from the user database (`getpwuid_r`).
fn account_shell() -> Option<String> {
    let mut buffer = vec![0 as libc::c_char; 4096];
    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: the buffers outlive the call; on success `result` points to `passwd`, whose strings
    // live in `buffer`.
    let status = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut passwd,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() || passwd.pw_shell.is_null() {
        return None;
    }
    // SAFETY: `pw_shell` is a NUL-terminated string inside `buffer`.
    let shell = unsafe { std::ffi::CStr::from_ptr(passwd.pw_shell) };
    shell
        .to_str()
        .ok()
        .filter(|shell| !shell.is_empty())
        .map(str::to_string)
}

/// Variables set for the program on top of Flux's own environment: the terminal type and color
/// support, the terminal's name, a UTF-8 locale if none is set (an app started from Finder has
/// none: then it follows the system's region, as in Terminal), then `extra`.
pub(crate) fn environment(extra: &[(String, String)]) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    vars.insert("TERM".into(), "xterm-256color".into());
    vars.insert("COLORTERM".into(), "truecolor".into());
    vars.insert("TERM_PROGRAM".into(), "Flux".into());
    vars.insert(
        "TERM_PROGRAM_VERSION".into(),
        env!("CARGO_PKG_VERSION").into(),
    );
    let has_locale = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .any(|name| env::var(name).is_ok_and(|value| !value.is_empty()));
    if !has_locale {
        vars.insert("LANG".into(), system_lang().to_string());
    }
    vars.extend(extra.iter().cloned());
    vars
}

/// The UTF-8 locale of the system's region and language (macOS `AppleLocale`, e.g. `ru_RU` →
/// `ru_RU.UTF-8`), if the system has it; otherwise `en_US.UTF-8`. Read once.
fn system_lang() -> &'static str {
    static LANG: OnceLock<String> = OnceLock::new();
    LANG.get_or_init(|| {
        apple_locale()
            .and_then(|locale| {
                locale_candidates(&locale)
                    .into_iter()
                    .find(|name| Path::new("/usr/share/locale").join(name).is_dir())
            })
            .unwrap_or_else(|| FALLBACK_LANG.to_string())
    })
}

const FALLBACK_LANG: &str = "en_US.UTF-8";

#[cfg(target_os = "macos")]
fn apple_locale() -> Option<String> {
    let output = std::process::Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleLocale"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let locale = String::from_utf8(output.stdout).ok()?;
    Some(locale.trim().to_string()).filter(|locale| !locale.is_empty())
}

#[cfg(not(target_os = "macos"))]
fn apple_locale() -> Option<String> {
    None
}

/// POSIX locales to try for an Apple locale: `ru_RU@rg=…` → `ru_RU.UTF-8`; a language with a region
/// it has no locale for (`ru_US`, `en_RU`) falls back to the language's own country (`ru_RU`).
fn locale_candidates(apple: &str) -> Vec<String> {
    let name = apple
        .split('@')
        .next()
        .unwrap_or_default()
        .replace('-', "_");
    let language = name.split('_').next().unwrap_or_default();
    let mut candidates = Vec::new();
    if name.contains('_') {
        candidates.push(format!("{name}.UTF-8"));
    }
    let own_country = format!("{language}_{}.UTF-8", language.to_ascii_uppercase());
    if language.len() == 2
        && language.chars().all(|c| c.is_ascii_lowercase())
        && !candidates.contains(&own_country)
    {
        candidates.push(own_country);
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locales_from_apple_locale() {
        assert_eq!(locale_candidates("ru_RU"), ["ru_RU.UTF-8"]);
        assert_eq!(
            locale_candidates("en_RU@currency=RUB"),
            ["en_RU.UTF-8", "en_EN.UTF-8"]
        );
        assert_eq!(locale_candidates("de"), ["de_DE.UTF-8"]);
        assert_eq!(
            locale_candidates("zh-Hans_CN"),
            ["zh_Hans_CN.UTF-8", "zh_ZH.UTF-8"]
        );
    }

    #[test]
    fn the_system_lang_is_a_utf8_locale_that_exists() {
        let lang = system_lang();
        assert!(lang.ends_with(".UTF-8"), "{lang}");
        if cfg!(target_os = "macos") {
            assert!(Path::new("/usr/share/locale").join(lang).is_dir(), "{lang}");
        }
    }

    #[test]
    fn the_login_shell_is_an_absolute_path() {
        let (shell, args) = login_shell();
        assert!(shell.starts_with('/'), "{shell}");
        assert_eq!(args, ["-l"]);
    }
}
