//! Edits Claude proposes (Edit, Write, NotebookEdit): what the file would become, and the
//! permission answer for the user's version of it — unchanged, trimmed hunk by hunk in the diff, or
//! edited by hand.

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

/// A proposed change of one file, as the diff shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct EditProposal {
    pub path: PathBuf,
    /// The file's text now; `None` — it doesn't exist yet (Write creates it).
    pub original: Option<String>,
    /// The text after the edit; an error when the edit doesn't apply (its `old_string` isn't in
    /// the file any more).
    pub proposed: Result<String, String>,
}

/// The tools whose permission request is an edit proposal.
pub fn is_edit_tool(tool: &str) -> bool {
    matches!(tool, "Edit" | "MultiEdit" | "Write")
}

/// The proposal of an edit request, reading the file from disk.
pub fn proposal(tool: &str, input: &Value) -> Option<EditProposal> {
    let path = PathBuf::from(input["file_path"].as_str()?);
    let original = fs::read_to_string(&path).ok();
    let proposed = proposed_text(tool, input, original.as_deref());
    Some(EditProposal {
        path,
        original,
        proposed,
    })
}

/// What the file becomes after the edit.
pub fn proposed_text(tool: &str, input: &Value, original: Option<&str>) -> Result<String, String> {
    match tool {
        "Write" => Ok(input["content"].as_str().unwrap_or("").to_string()),
        "Edit" => {
            let original = original.ok_or("The file doesn't exist")?;
            apply_edit(
                original,
                input["old_string"].as_str().unwrap_or(""),
                input["new_string"].as_str().unwrap_or(""),
                input["replace_all"].as_bool().unwrap_or(false),
            )
        }
        "MultiEdit" => {
            let mut text = original.ok_or("The file doesn't exist")?.to_string();
            for edit in input["edits"].as_array().into_iter().flatten() {
                text = apply_edit(
                    &text,
                    edit["old_string"].as_str().unwrap_or(""),
                    edit["new_string"].as_str().unwrap_or(""),
                    edit["replace_all"].as_bool().unwrap_or(false),
                )?;
            }
            Ok(text)
        }
        _ => Err(format!("{tool} is not an edit")),
    }
}

/// The file with an edit of Claude undone, when the file changed after it (part 10.2): every
/// replacement goes back if its new text is still in the file exactly once. A `Write`, a
/// replace-all, a text that's gone or repeated can't be undone this way.
pub fn reverted_text(tool: &str, input: &Value, current: &str) -> Result<String, String> {
    let edits: Vec<&Value> = match tool {
        "Edit" => vec![input],
        "MultiEdit" => input["edits"].as_array().into_iter().flatten().collect(),
        _ => return Err(format!("{tool} replaces the whole file")),
    };
    let mut text = current.to_string();
    // The last replacement first: a later one may sit inside an earlier one's text.
    for edit in edits.iter().rev() {
        if edit["replace_all"].as_bool().unwrap_or(false) {
            return Err("The edit replaced every occurrence".into());
        }
        let old = edit["old_string"].as_str().unwrap_or("");
        let new = edit["new_string"].as_str().unwrap_or("");
        if new.is_empty() {
            return Err("The edit removed text: there is nothing to find it by".into());
        }
        text = match text.matches(new).count() {
            1 => text.replacen(new, old, 1),
            0 => return Err("The edited text isn't in the file any more".into()),
            _ => return Err("The edited text occurs more than once".into()),
        };
    }
    Ok(text)
}

fn apply_edit(text: &str, old: &str, new: &str, replace_all: bool) -> Result<String, String> {
    if old.is_empty() {
        return Err("The edit has no text to replace".into());
    }
    match text.matches(old).count() {
        0 => Err("The text to replace isn't in the file any more".into()),
        1 => Ok(text.replacen(old, new, 1)),
        _ if replace_all => Ok(text.replace(old, new)),
        _ => Err("The text to replace occurs more than once".into()),
    }
}

/// The input to allow the edit with, when the user's version of the file is `text`: the original
/// input when it is the proposal unchanged, otherwise an input that writes `text`.
pub fn updated_input(
    tool: &str,
    input: &Value,
    original: Option<&str>,
    proposed: Option<&str>,
    text: &str,
) -> Value {
    if proposed == Some(text) {
        return input.clone();
    }
    let path = input["file_path"].clone();
    match (tool, original) {
        // The whole file as the replaced text: unique by definition.
        ("Edit" | "MultiEdit", Some(original)) if !original.is_empty() => {
            let mut changed = json!({
                "file_path": path,
                "old_string": original,
                "new_string": text,
                "replace_all": false,
            });
            if tool == "MultiEdit" {
                changed = json!({
                    "file_path": path,
                    "edits": [{ "old_string": original, "new_string": text, "replace_all": false }],
                });
            }
            changed
        }
        _ => json!({ "file_path": path, "content": text }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_apply_once_or_everywhere() {
        let input = json!({ "file_path": "/x", "old_string": "b", "new_string": "B" });
        assert_eq!(proposed_text("Edit", &input, Some("abc")).unwrap(), "aBc");
        assert!(proposed_text("Edit", &input, Some("bb")).is_err());
        let all =
            json!({ "file_path": "/x", "old_string": "b", "new_string": "B", "replace_all": true });
        assert_eq!(proposed_text("Edit", &all, Some("bb")).unwrap(), "BB");
        assert!(proposed_text("Edit", &input, Some("ac")).is_err());
        assert!(proposed_text("Edit", &input, None).is_err());
        let write = json!({ "file_path": "/x", "content": "new" });
        assert_eq!(proposed_text("Write", &write, None).unwrap(), "new");
    }

    #[test]
    fn an_edit_is_undone_where_its_text_still_is() {
        let edit = json!({ "file_path": "/x", "old_string": "port", "new_string": "env port" });
        // The file changed elsewhere after the edit: the edit alone goes back.
        assert_eq!(
            reverted_text("Edit", &edit, "fn a() {}\nenv port = 1\n").unwrap(),
            "fn a() {}\nport = 1\n"
        );
        assert!(reverted_text("Edit", &edit, "port = 1").is_err());
        assert!(reverted_text("Edit", &edit, "env port; env port").is_err());
        let multi = json!({ "file_path": "/x", "edits": [
            { "old_string": "a", "new_string": "AB" },
            { "old_string": "B", "new_string": "Bc" },
        ]});
        // "a" → "AB" → "ABc": undone last first.
        assert_eq!(reverted_text("MultiEdit", &multi, "xABcx").unwrap(), "xax");
        let write = json!({ "file_path": "/x", "content": "new" });
        assert!(reverted_text("Write", &write, "new").is_err());
        let all =
            json!({ "file_path": "/x", "old_string": "b", "new_string": "B", "replace_all": true });
        assert!(reverted_text("Edit", &all, "BB").is_err());
    }

    #[test]
    fn an_unchanged_proposal_keeps_the_input() {
        let input = json!({ "file_path": "/x", "old_string": "b", "new_string": "B" });
        assert_eq!(
            updated_input("Edit", &input, Some("abc"), Some("aBc"), "aBc"),
            input
        );
        let changed = updated_input("Edit", &input, Some("abc"), Some("aBc"), "aXc");
        assert_eq!(changed["old_string"], "abc");
        assert_eq!(changed["new_string"], "aXc");
        let write = json!({ "file_path": "/x", "content": "new" });
        assert_eq!(
            updated_input("Write", &write, None, Some("new"), "newer")["content"],
            "newer"
        );
    }
}
