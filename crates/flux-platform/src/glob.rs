//! File name patterns of languages and their servers (`file-patterns`, plugin API 0.4): globs over
//! a file's name, not its path. `*` is any run of characters, `?` one character, `[…]` one of a set (`[abc]`, `[a-z]`,
//! `[!0-9]` or `[^0-9]` — none of them); every other character is itself. Letters match in any case,
//! as extensions do.

/// Whether `name` (a file name, without its folder) matches `pattern`. A pattern that isn't valid
/// ([`check_glob`]) matches nothing.
pub fn glob_matches(pattern: &str, name: &str) -> bool {
    if check_glob(pattern).is_err() {
        return false;
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    matches(&pattern, &name)
}

/// Whether `pattern` is a pattern of a file name: not empty, no `/` or `\` (it is matched against a
/// name, not a path), every `[` closed by a `]` with something between them.
pub fn check_glob(pattern: &str) -> Result<(), String> {
    if pattern.is_empty() {
        return Err("an empty pattern".into());
    }
    if pattern.contains(['/', '\\']) {
        return Err(format!(
            "\"{pattern}\" — a pattern of a file name, without folders"
        ));
    }
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            match class_end(&chars, i) {
                Some(end) => i = end,
                None => return Err(format!("\"{pattern}\" — a `[` without its `]`")),
            }
        } else {
            i += 1;
        }
    }
    Ok(())
}

/// Backtracking over the last `*`: linear for patterns with one `*`, at worst
/// `pattern × name` for more.
fn matches(pattern: &[char], name: &[char]) -> bool {
    let (mut p, mut n) = (0, 0);
    // The last `*` and the position in the name it has swallowed up to.
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        let step = match pattern.get(p) {
            Some('*') => {
                star = Some((p, n));
                p += 1;
                continue;
            }
            Some('?') => Some(p + 1),
            Some('[') => {
                let end = class_end(pattern, p).unwrap_or(p + 1);
                class_matches(&pattern[p + 1..end - 1], name[n]).then_some(end)
            }
            Some(&c) => same(c, name[n]).then_some(p + 1),
            None => None,
        };
        match (step, star) {
            (Some(next), _) => {
                p = next;
                n += 1;
            }
            (None, Some((star_at, swallowed))) => {
                p = star_at + 1;
                n = swallowed + 1;
                star = Some((star_at, n));
            }
            (None, None) => return false,
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

/// The index after the `]` closing the class that starts at `start` (a `[`). A `]` right after `[`
/// or `[!` is a member, not the end.
fn class_end(pattern: &[char], start: usize) -> Option<usize> {
    let mut i = start + 1;
    if matches!(pattern.get(i), Some('!' | '^')) {
        i += 1;
    }
    if pattern.get(i) == Some(&']') {
        i += 1;
    }
    while i < pattern.len() {
        if pattern[i] == ']' {
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

/// `class` is what is between `[` and `]`.
fn class_matches(class: &[char], c: char) -> bool {
    let (negated, class) = match class.first() {
        Some('!' | '^') => (true, &class[1..]),
        _ => (false, class),
    };
    let mut found = false;
    let mut i = 0;
    while i < class.len() {
        if i + 2 < class.len() && class[i + 1] == '-' {
            let (low, high) = (class[i], class[i + 2]);
            found |= (low..=high).contains(&c)
                || (low..=high).contains(&c.to_ascii_lowercase())
                || (low..=high).contains(&c.to_ascii_uppercase());
            i += 3;
        } else {
            found |= same(class[i], c);
            i += 1;
        }
    }
    found != negated
}

fn same(a: char, b: char) -> bool {
    a.eq_ignore_ascii_case(&b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stars_and_question_marks() {
        assert!(glob_matches(".env.*", ".env.local"));
        assert!(glob_matches(".env.*", ".env."));
        assert!(!glob_matches(".env.*", ".env"));
        assert!(glob_matches("*.env", "prod.env"));
        assert!(glob_matches("*.env", ".env"));
        assert!(!glob_matches("*.env", "prod.env.bak"));
        assert!(glob_matches("*.component.html", "app.component.html"));
        assert!(!glob_matches("*.component.html", "app.html"));
        assert!(glob_matches("Dockerfile.*", "dockerfile.dev"), "any case");
        assert!(glob_matches("*.*.*", "a.b.c"));
        assert!(glob_matches("a*b*c", "aXXbYYbc"));
        assert!(!glob_matches("a*b*c", "aXXbYYbd"));
        assert!(glob_matches("file?.txt", "file1.txt"));
        assert!(!glob_matches("file?.txt", "file.txt"));
        assert!(glob_matches("*", "anything"));
        assert!(glob_matches("ы*.md", "ыЫ.md"));
        assert!(
            !glob_matches("ы*.md", "Ыы.md"),
            "other letters match exactly"
        );
    }

    #[test]
    fn classes() {
        assert!(glob_matches("[Mm]akefile", "makefile"));
        assert!(glob_matches("v[0-9].txt", "v7.txt"));
        assert!(!glob_matches("v[0-9].txt", "vx.txt"));
        assert!(glob_matches("v[!0-9].txt", "vx.txt"));
        assert!(glob_matches("v[^0-9].txt", "vx.txt"));
        assert!(!glob_matches("v[!0-9].txt", "v1.txt"));
        assert!(glob_matches("[]]", "]"));
        assert!(glob_matches("[a-c]*", "Beta"), "a range in any case");
    }

    #[test]
    fn invalid_patterns_match_nothing() {
        assert!(check_glob("").is_err());
        assert!(check_glob("src/*.rs").is_err());
        assert!(check_glob("a\\b").is_err());
        assert!(check_glob("[abc").is_err());
        assert!(check_glob("[]").is_err());
        assert!(!glob_matches("[abc", "[abc"));
        assert!(check_glob(".env.*").is_ok());
        assert!(check_glob("[!a]x").is_ok());
    }
}
