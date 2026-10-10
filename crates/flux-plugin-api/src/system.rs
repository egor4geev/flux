//! What is outside the window: the browser, the clipboard, the user's home folder (where the
//! folders of the manifest's `folders` permission are).
//!
//! ```ignore
//! system::open_url("https://tracker.example.com/issue/FLUX-12")?;
//! system::copy_text("FLUX-12");
//! let config = std::fs::read_to_string(system::expand_home("~/.config/gh/hosts.yml"))?;
//! ```

pub use crate::host::system::{copy_text, home_dir, open_url};

/// A path with `~` for the home folder made absolute: `~/.config/gh` → `/Users/me/.config/gh`.
/// Other paths stay as they are.
pub fn expand_home(path: &str) -> String {
    expand(&home_dir(), path)
}

fn expand(home: &str, path: &str) -> String {
    match path.strip_prefix('~') {
        Some("") => home.to_string(),
        Some(rest) if rest.starts_with('/') => format!("{}{rest}", home.trim_end_matches('/')),
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::expand;

    #[test]
    fn expands_the_home_folder() {
        assert_eq!(expand("/Users/me", "~/.config/gh"), "/Users/me/.config/gh");
        assert_eq!(expand("/Users/me/", "~"), "/Users/me/");
        assert_eq!(expand("/Users/me", "~other/x"), "~other/x");
        assert_eq!(expand("/Users/me", "/etc/hosts"), "/etc/hosts");
    }
}
