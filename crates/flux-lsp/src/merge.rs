//! A file's several servers answer one request (plugin API 0.4: a framework's classes, a linter
//! next to the language's own server): completion items and hovers of the main server and of the
//! secondary ones made one answer — the main server's first.

use lsp_types::{
    CompletionItem, CompletionResponse, Hover, HoverContents, MarkedString, MarkupContent,
    MarkupKind,
};

/// The items of several servers' completion answers, in the order given (the main server's first),
/// each with the index of the answer it came from — it is resolved and applied by that server. An
/// item of a later server with the label and the text of an earlier one's is dropped. `true` — one
/// of the lists is incomplete: typing asks again.
pub fn completions(
    answers: Vec<(usize, CompletionResponse)>,
) -> (Vec<(usize, CompletionItem)>, bool) {
    let mut items: Vec<(usize, CompletionItem)> = Vec::new();
    let mut incomplete = false;
    for (source, answer) in answers {
        let list = match answer {
            CompletionResponse::Array(items) => items,
            CompletionResponse::List(list) => {
                incomplete |= list.is_incomplete;
                list.items
            }
        };
        let earlier = items.len();
        for item in list {
            let duplicate = items[..earlier].iter().any(|(from, known)| {
                *from != source && known.label == item.label && inserted(known) == inserted(&item)
            });
            if !duplicate {
                items.push((source, item));
            }
        }
    }
    (items, incomplete)
}

/// The text an item inserts, as far as it can be told without the document.
fn inserted(item: &CompletionItem) -> &str {
    match &item.text_edit {
        Some(lsp_types::CompletionTextEdit::Edit(edit)) => &edit.new_text,
        Some(lsp_types::CompletionTextEdit::InsertAndReplace(edit)) => &edit.new_text,
        None => item.insert_text.as_deref().unwrap_or(&item.label),
    }
}

/// Several servers' hovers as one, in the order given (the main server's first), separated by a
/// rule; empty ones are dropped. The range is the first one's that has a range.
pub fn hovers(answers: Vec<Hover>) -> Option<Hover> {
    let mut answers: Vec<Hover> = answers
        .into_iter()
        .filter(|hover| !is_empty(&hover.contents))
        .collect();
    if answers.len() <= 1 {
        return answers.pop();
    }
    let range = answers.iter().find_map(|hover| hover.range);
    let value = answers
        .into_iter()
        .map(|hover| markdown(hover.contents))
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range,
    })
}

fn is_empty(contents: &HoverContents) -> bool {
    let blank = |marked: &MarkedString| match marked {
        MarkedString::String(text) => text.trim().is_empty(),
        MarkedString::LanguageString(code) => code.value.trim().is_empty(),
    };
    match contents {
        HoverContents::Scalar(marked) => blank(marked),
        HoverContents::Array(items) => items.iter().all(blank),
        HoverContents::Markup(markup) => markup.value.trim().is_empty(),
    }
}

/// Hover contents as Markdown: code in fences, plain text with Markdown's characters escaped.
fn markdown(contents: HoverContents) -> String {
    let marked = |marked: MarkedString| match marked {
        MarkedString::String(text) => text,
        MarkedString::LanguageString(code) => {
            format!(
                "```{}\n{}\n```",
                code.language,
                code.value.trim_end_matches('\n')
            )
        }
    };
    match contents {
        HoverContents::Scalar(item) => marked(item),
        HoverContents::Array(items) => items
            .into_iter()
            .map(marked)
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }) => value,
        HoverContents::Markup(MarkupContent { value, .. }) => escape(&value),
    }
}

/// Plain text that reads the same as Markdown.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(
            c,
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '|'
        ) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped.replace('\n', "  \n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{CompletionList, LanguageString, Position, Range};

    fn item(label: &str) -> CompletionItem {
        CompletionItem {
            label: label.into(),
            ..Default::default()
        }
    }

    #[test]
    fn completion_items_keep_their_server_and_lose_duplicates() {
        let main = CompletionResponse::Array(vec![item("flex"), item("grid")]);
        let secondary = CompletionResponse::List(CompletionList {
            is_incomplete: true,
            items: vec![
                item("flex"),
                CompletionItem {
                    insert_text: Some("grid-cols".into()),
                    ..item("grid")
                },
                item("flex-col"),
                item("flex-col"),
            ],
        });
        let (items, incomplete) = completions(vec![(0, main), (1, secondary)]);
        let labels: Vec<(usize, &str)> = items
            .iter()
            .map(|(source, item)| (*source, item.label.as_str()))
            .collect();
        // The second server's `flex` is the first one's; its `grid` inserts other text. Its own
        // two `flex-col` are its business.
        assert_eq!(
            labels,
            [
                (0, "flex"),
                (0, "grid"),
                (1, "grid"),
                (1, "flex-col"),
                (1, "flex-col")
            ]
        );
        assert!(incomplete);
        let (_, incomplete) = completions(vec![(0, CompletionResponse::Array(Vec::new()))]);
        assert!(!incomplete);
    }

    #[test]
    fn hovers_join_the_main_servers_first_and_drop_empty_ones() {
        let range = Range::new(Position::new(0, 1), Position::new(0, 4));
        let main = Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: "**main**".into(),
            }),
            range: None,
        };
        let empty = Hover {
            contents: HoverContents::Array(vec![MarkedString::String(" ".into())]),
            range: Some(Range::default()),
        };
        let css = Hover {
            contents: HoverContents::Array(vec![MarkedString::LanguageString(LanguageString {
                language: "css".into(),
                value: ".flex {\n  display: flex;\n}\n".into(),
            })]),
            range: Some(range),
        };
        let plain = Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::PlainText,
                value: "a_b".into(),
            }),
            range: None,
        };
        let joined = hovers(vec![main.clone(), empty.clone(), css, plain]).unwrap();
        assert_eq!(joined.range, Some(range));
        let HoverContents::Markup(markup) = joined.contents else {
            panic!("markup expected");
        };
        assert_eq!(
            markup.value,
            "**main**\n\n---\n\n```css\n.flex {\n  display: flex;\n}\n```\n\n---\n\na\\_b"
        );
        // One left: as it is.
        assert_eq!(hovers(vec![empty.clone(), main.clone()]), Some(main));
        assert_eq!(hovers(vec![empty]), None);
        assert_eq!(hovers(Vec::new()), None);
    }
}
