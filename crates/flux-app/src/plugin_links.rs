//! `links` (plugin API 0.3, part 10.3): the plugins' patterns of links on text — a task's key,
//! `TRACK-142` — matched in the editor (⌘ + hover underlines, ⌘-click), in commit messages (the
//! commit's details, the Git log), in Claude's chat and in Markdown (Claude's answers, the plugins'
//! windows, hover popups). A click goes back to the pattern's plugin as `link-activated`; a hover
//! asks it for a tooltip (`link-hovered` → `links.describe`, Markdown), which shows when it comes —
//! nothing after [`TOOLTIP_TIMEOUT`].
//!
//! The patterns live in a registry of the UI thread, each under the plugin hub of the window that
//! set it: the editor matches its own window's (its document's hub), and a click goes to the plugin
//! running in the window where it happened (each window runs its own). Text drawn without a
//! context — Markdown, a commit's message — matches every window's patterns, once. A plugin's
//! patterns go when it stops; a closed window's are skipped.
//!
//! Plain web links of Markdown (`[text](https://…)`, `<https://…>`, a bare `https://…`) open in the
//! browser through [`open_url`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use flux_plugin::api::events::Event;
use flux_plugin::api::links::LinkPattern;
use gpui::{
    AnyElement, AnyView, App, Context, ElementId, Entity, EntityId, Hsla, InteractiveText,
    IntoElement, Render, SharedString, StyledText, TextRun, UnderlineStyle, WeakEntity, Window,
    div, prelude::*, px,
};
use regex::Regex;

use crate::markdown::{self, Block};
use crate::plugins::PluginStore;
use crate::theme::{self, Theme};
use crate::workspace::Workspace;

/// How long a hovered link waits for its plugin's tooltip.
const TOOLTIP_TIMEOUT: Duration = Duration::from_secs(3);
const TOOLTIP_WIDTH: f32 = 420.;

/// A pattern a plugin set.
struct Pattern {
    /// The hub of the window whose plugin set it.
    store: EntityId,
    plugin: Arc<str>,
    id: String,
    regex: Regex,
    tooltip: bool,
}

/// What a hovered link's tooltip does with its plugin's answer.
type Answer = Box<dyn FnOnce(Option<String>, &mut App)>;

#[derive(Default)]
struct Registry {
    patterns: Vec<Rc<Pattern>>,
    /// The windows' plugin hubs that set patterns: where clicks and hovers go.
    stores: HashMap<EntityId, WeakEntity<PluginStore>>,
    next_request: u64,
    /// Tooltips waiting for their plugin's `links.describe`: (the plugin, what to do with it).
    waiting: HashMap<u64, (Arc<str>, Answer)>,
}

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
}

/// A plugin's link found in a text.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Link {
    /// Bytes of the text it was found in.
    pub range: Range<usize>,
    pub plugin: Arc<str>,
    pub pattern: String,
    pub text: String,
    pub tooltip: bool,
    /// The hub of the window whose plugin set the pattern.
    store: EntityId,
}

/// `links.set-patterns`: replaces the plugin's patterns in the workspace's window; an error names
/// a regex that doesn't compile (the old patterns stay then).
pub(crate) fn set_patterns(
    workspace: &Workspace,
    plugin: &str,
    patterns: &[LinkPattern],
    cx: &mut App,
) -> Result<(), String> {
    let store = workspace.plugins.downgrade();
    let store_id = store.entity_id();
    let mut compiled = Vec::with_capacity(patterns.len());
    for pattern in patterns {
        let regex = Regex::new(&pattern.regex).map_err(|err| {
            format!(
                "links.set-patterns: the pattern \"{}\" doesn't compile: {err}",
                pattern.id
            )
        })?;
        compiled.push(Rc::new(Pattern {
            store: store_id,
            plugin: Arc::from(plugin),
            id: pattern.id.clone(),
            regex,
            tooltip: pattern.tooltip,
        }));
    }
    REGISTRY.with_borrow_mut(|registry| {
        // Closed windows' hubs and patterns go now.
        registry.stores.retain(|_, hub| hub.upgrade().is_some());
        let stores = &registry.stores;
        registry.patterns.retain(|known| {
            stores.contains_key(&known.store)
                && !(known.store == store_id && &*known.plugin == plugin)
        });
        registry.patterns.extend(compiled);
        registry.stores.insert(store_id, store);
    });
    cx.refresh_windows();
    Ok(())
}

/// A plugin stopped in a window: its patterns go there, and its tooltips won't come.
pub(crate) fn forget(store: EntityId, plugin: &str, cx: &mut App) {
    let removed = REGISTRY.with_borrow_mut(|registry| {
        let before = registry.patterns.len();
        registry
            .patterns
            .retain(|pattern| !(pattern.store == store && &*pattern.plugin == plugin));
        before != registry.patterns.len()
    });
    if removed {
        cx.refresh_windows();
    }
}

/// `links.describe`: the plugin's tooltip for the hovered link `request`.
pub(crate) fn describe(plugin: &str, request: u64, markdown: Option<String>, cx: &mut App) {
    let answer = REGISTRY.with_borrow_mut(|registry| {
        let mine = registry
            .waiting
            .get(&request)
            .is_some_and(|(owner, _)| &**owner == plugin);
        mine.then(|| registry.waiting.remove(&request))
            .flatten()
            .map(|(_, answer)| answer)
    });
    if let Some(answer) = answer {
        answer(markdown.filter(|text| !text.trim().is_empty()), cx);
    }
}

/// The plugin hub of a window: its links are the ones the window shows.
pub(crate) fn store_of(window: &Window, cx: &App) -> Option<EntityId> {
    let workspace = window.root::<Workspace>().flatten()?;
    Some(workspace.read(cx).plugins.entity_id())
}

/// The links in `text` by the patterns of the window's hub `store` (none — every window's, the
/// same ones once): by their start, without overlaps (an earlier or longer one wins).
pub(crate) fn find(store: Option<EntityId>, text: &str) -> Vec<Link> {
    if text.is_empty() {
        return Vec::new();
    }
    let patterns: Vec<Rc<Pattern>> = REGISTRY.with_borrow(|registry| {
        // A closed window's patterns are left out.
        registry
            .patterns
            .iter()
            .filter(|pattern| {
                registry
                    .stores
                    .get(&pattern.store)
                    .is_some_and(|hub| hub.upgrade().is_some())
            })
            .cloned()
            .collect()
    });
    find_in(&patterns, store, text)
}

fn find_in(patterns: &[Rc<Pattern>], store: Option<EntityId>, text: &str) -> Vec<Link> {
    let mut links: Vec<Link> = Vec::new();
    for pattern in patterns
        .iter()
        .filter(|pattern| store.is_none_or(|store| pattern.store == store))
    {
        for found in pattern.regex.find_iter(text) {
            if found.range().is_empty() {
                continue;
            }
            let duplicate = links.iter().any(|link| {
                link.range == found.range()
                    && link.plugin == pattern.plugin
                    && link.pattern == pattern.id
            });
            if !duplicate {
                links.push(Link {
                    range: found.range(),
                    plugin: pattern.plugin.clone(),
                    pattern: pattern.id.clone(),
                    text: found.as_str().to_string(),
                    tooltip: pattern.tooltip,
                    store: pattern.store,
                });
            }
        }
    }
    without_overlaps(links)
}

/// Sorted by start; of overlapping links the earlier one stays, of two at one place the longer.
fn without_overlaps(mut links: Vec<Link>) -> Vec<Link> {
    links.sort_by(|a, b| {
        a.range
            .start
            .cmp(&b.range.start)
            .then(b.range.end.cmp(&a.range.end))
    });
    let mut kept: Vec<Link> = Vec::with_capacity(links.len());
    for link in links {
        if kept
            .last()
            .is_none_or(|last| link.range.start >= last.range.end)
        {
            kept.push(link);
        }
    }
    kept
}

/// A live hub by its id.
fn store_entity(store: EntityId) -> Option<Entity<PluginStore>> {
    REGISTRY.with_borrow(|registry| registry.stores.get(&store)?.upgrade())
}

/// The hub a link's events go to: the window's where it was clicked or hovered (`store`), when its
/// plugin set patterns there too, else the one that set the link's pattern.
fn target(link: &Link, store: Option<EntityId>) -> Option<Entity<PluginStore>> {
    let here = store.filter(|store| {
        REGISTRY.with_borrow(|registry| {
            registry
                .patterns
                .iter()
                .any(|pattern| pattern.store == *store && pattern.plugin == link.plugin)
        })
    });
    here.and_then(store_entity)
        .or_else(|| store_entity(link.store))
}

/// A click on a link: `link-activated` to its plugin in the window `store` (where it was clicked).
pub(crate) fn activate(link: &Link, store: Option<EntityId>, cx: &mut App) {
    #[cfg(feature = "scenario")]
    println!("LINK {} {} {}", link.plugin, link.pattern, link.text);
    if let Some(target) = target(link, store) {
        target.read(cx).send_to(
            &link.plugin,
            Event::LinkActivated((link.pattern.clone(), link.text.clone())),
        );
    }
}

/// Asks the link's plugin for a tooltip: `answer` gets its Markdown (none — nothing to show, or
/// no answer in [`TOOLTIP_TIMEOUT`]). False when the pattern has no tooltips or its plugin is gone.
pub(crate) fn request_tooltip(
    link: &Link,
    store: Option<EntityId>,
    cx: &mut App,
    answer: impl FnOnce(Option<String>, &mut App) + 'static,
) -> bool {
    if !link.tooltip {
        return false;
    }
    let Some(target) = target(link, store) else {
        return false;
    };
    let request = REGISTRY.with_borrow_mut(|registry| {
        registry.next_request += 1;
        let request = registry.next_request;
        registry
            .waiting
            .insert(request, (link.plugin.clone(), Box::new(answer)));
        request
    });
    target.read(cx).send_to(
        &link.plugin,
        Event::LinkHovered((request, link.pattern.clone(), link.text.clone())),
    );
    cx.spawn(async move |cx| {
        cx.background_executor().timer(TOOLTIP_TIMEOUT).await;
        cx.update(|cx| {
            let answer = REGISTRY
                .with_borrow_mut(|registry| registry.waiting.remove(&request))
                .map(|(_, answer)| answer);
            if let Some(answer) = answer {
                answer(None, cx);
            }
        })
        .ok();
    })
    .detach();
    true
}

/// The tooltip of a link outside the editor (a commit message, the chat, Markdown): empty until the
/// plugin answers, then its Markdown in a popup; or a web link's address.
pub(crate) struct LinkTooltip {
    blocks: Option<Vec<Block>>,
}

impl Render for LinkTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(blocks) = self.blocks.as_ref().filter(|blocks| !blocks.is_empty()) else {
            return div().into_any_element();
        };
        let theme = Theme::get(cx);
        let ui = theme.ui;
        // gpui puts the tooltip at the mouse point; the offset moves it out from under the cursor.
        div()
            .pl(px(10.))
            .pt(px(18.))
            .child(
                crate::popup::panel(ui)
                    .max_w(px(TOOLTIP_WIDTH))
                    .px_3()
                    .py_2()
                    .text_size(px(theme::TEXT_SM))
                    .child(markdown::render(blocks, ui.foreground, theme)),
            )
            .into_any_element()
    }
}

/// A link's tooltip view for gpui's tooltips (`InteractiveText::tooltip`): asks the plugin now.
pub(crate) fn tooltip_view(link: &Link, store: Option<EntityId>, cx: &mut App) -> Option<AnyView> {
    if !link.tooltip {
        return None;
    }
    let view = cx.new(|_| LinkTooltip { blocks: None });
    let weak = view.downgrade();
    let asked = request_tooltip(link, store, cx, move |markdown, cx| {
        weak.update(cx, |tooltip, cx| {
            tooltip.blocks = markdown.map(|markdown| markdown::parse(&markdown));
            cx.notify();
        })
        .ok();
    });
    asked.then(|| view.into())
}

/// A web link's tooltip: its address, as browsers show it.
pub(crate) fn url_tooltip(url: &str, cx: &mut App) -> AnyView {
    let blocks = vec![Block::Paragraph(vec![markdown::Span::plain(url)])];
    cx.new(|_| LinkTooltip {
        blocks: Some(blocks),
    })
    .into()
}

/// Opens a web link of Markdown in the browser — in a scenario with `FLUX_SCENARIO_OPEN_URL`, a
/// line in that file instead (the author's browser stays alone).
pub(crate) fn open_url(url: &str, cx: &mut App) {
    #[cfg(feature = "scenario")]
    if let Some(file) = std::env::var_os("FLUX_SCENARIO_OPEN_URL") {
        use std::io::Write as _;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
        {
            let _ = writeln!(file, "{url}");
        }
        return;
    }
    cx.open_url(url);
}

/// Bare web addresses in plain text (`https://…`, `http://…`): their byte ranges, without the
/// punctuation that ends a sentence after them and a closing bracket the address didn't open.
pub(crate) fn web_links(text: &str) -> Vec<Range<usize>> {
    thread_local! {
        static URL: Regex = Regex::new(r#"https?://[^\s<>"'`]+"#).expect("a valid regex");
    }
    if !text.contains("://") {
        return Vec::new();
    }
    URL.with(|url| {
        url.find_iter(text)
            .filter_map(|found| {
                let mut end = found.end();
                loop {
                    let address = &text[found.start()..end];
                    let Some(last) = address.chars().last() else {
                        break;
                    };
                    let unopened = |open: char| {
                        address.matches(open).count() < address.matches(last).count()
                    };
                    let trim = match last {
                        '.' | ',' | ';' | ':' | '!' | '?' | '*' | '_' => true,
                        ')' => unopened('('),
                        ']' => unopened('['),
                        '}' => unopened('{'),
                        _ => false,
                    };
                    if !trim {
                        break;
                    }
                    end -= last.len_utf8();
                }
                // Something after the scheme: not a lone `https://`.
                (end > found.start() + "https://".len()).then(|| found.start()..end)
            })
            .collect()
    })
}

/// The runs of a text with these byte ranges drawn as links: `color`, underlined.
pub(crate) fn link_runs(runs: Vec<TextRun>, ranges: &[Range<usize>], color: Hsla) -> Vec<TextRun> {
    if ranges.is_empty() {
        return runs;
    }
    let mut out = Vec::with_capacity(runs.len() + ranges.len() * 2);
    let mut start = 0;
    for run in runs {
        let end = start + run.len;
        // Cut the run where a link starts or ends inside it.
        let mut cuts: Vec<usize> = ranges
            .iter()
            .flat_map(|range| [range.start, range.end])
            .filter(|&cut| cut > start && cut < end)
            .collect();
        cuts.sort_unstable();
        cuts.dedup();
        let mut from = start;
        for to in cuts.into_iter().chain(std::iter::once(end)) {
            let inside = ranges
                .iter()
                .any(|range| range.start <= from && to <= range.end);
            let mut piece = run.clone();
            piece.len = to - from;
            if inside {
                piece.color = color;
                piece.underline = Some(UnderlineStyle {
                    thickness: px(1.),
                    color: Some(color),
                    wavy: false,
                });
            }
            out.push(piece);
            from = to;
        }
        start = end;
    }
    out
}

/// What a clickable range of a text does.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Target {
    /// A plugin's link.
    Plugin(Link),
    /// A web link: opens in the browser.
    Url(String),
}

/// Text with clickable ranges (byte ranges of `text`, by their start, not overlapping): drawn as
/// links in `color` — plain styled text without any.
pub(crate) fn clickable_text(
    id: impl Into<ElementId>,
    text: SharedString,
    runs: Vec<TextRun>,
    targets: Vec<(Range<usize>, Target)>,
    color: Hsla,
) -> AnyElement {
    if targets.is_empty() {
        return StyledText::new(text).with_runs(runs).into_any_element();
    }
    let ranges: Vec<Range<usize>> = targets.iter().map(|(range, _)| range.clone()).collect();
    let runs = link_runs(runs, &ranges, color);
    let styled = StyledText::new(text).with_runs(runs);
    let targets = Rc::new(targets);
    let clicked = targets.clone();
    InteractiveText::new(id, styled)
        .on_click(ranges, move |index, window, cx| match clicked.get(index) {
            Some((_, Target::Plugin(link))) => {
                let store = store_of(window, cx);
                activate(link, store, cx);
            }
            Some((_, Target::Url(url))) => open_url(url, cx),
            None => {}
        })
        .tooltip(move |index, window, cx| {
            let (_, target) = targets.iter().find(|(range, _)| range.contains(&index))?;
            match target {
                Target::Plugin(link) => {
                    let store = store_of(window, cx);
                    tooltip_view(link, store, cx)
                }
                Target::Url(url) => Some(url_tooltip(url, cx)),
            }
        })
        .into_any_element()
}

/// The plugins' links of `text` as targets, leaving out those that overlap `taken` (byte ranges
/// already clickable).
pub(crate) fn plugin_targets(text: &str, taken: &[Range<usize>]) -> Vec<(Range<usize>, Target)> {
    targets(find(None, text), taken)
}

fn targets(links: Vec<Link>, taken: &[Range<usize>]) -> Vec<(Range<usize>, Target)> {
    links
        .into_iter()
        .filter(|link| {
            !taken
                .iter()
                .any(|range| range.start < link.range.end && link.range.start < range.end)
        })
        .map(|link| (link.range.clone(), Target::Plugin(link)))
        .collect()
}

/// Text in one font and color whose plugin links click and show tooltips: a commit's message.
pub(crate) fn linked_plain(
    id: impl Into<ElementId>,
    text: SharedString,
    font: gpui::Font,
    color: Hsla,
    cx: &App,
) -> AnyElement {
    let run = TextRun {
        len: text.len(),
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let targets = plugin_targets(&text, &[]);
    clickable_text(id, text, vec![run], targets, Theme::ui(cx).accent_text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(len: usize) -> TextRun {
        TextRun {
            len,
            font: gpui::font("Menlo"),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }
    }

    fn link(range: Range<usize>, pattern: &str) -> Link {
        Link {
            range,
            plugin: Arc::from("test.plugin"),
            pattern: pattern.into(),
            text: String::new(),
            tooltip: false,
            store: EntityId::from(1u64),
        }
    }

    #[test]
    fn overlapping_links_keep_the_earlier_and_longer() {
        let kept = without_overlaps(vec![
            link(10..14, "b"),
            link(0..5, "a"),
            link(0..8, "long"),
            link(6..12, "late"),
        ]);
        let ranges: Vec<_> = kept.iter().map(|link| link.range.clone()).collect();
        assert_eq!(ranges, vec![0..8, 10..14]);
        assert_eq!(kept[0].pattern, "long");
    }

    #[test]
    fn patterns_match_by_window() {
        let window_a = EntityId::from(7u64);
        let window_b = EntityId::from(8u64);
        let pattern = |store, id: &str, regex: &str| {
            Rc::new(Pattern {
                store,
                plugin: Arc::from("flux.yandex"),
                id: id.into(),
                regex: Regex::new(regex).unwrap(),
                tooltip: true,
            })
        };
        let patterns = vec![
            pattern(window_a, "task", r"\b(?:TRACK|WEB)-\d+\b"),
            // The same plugin in another window: its links aren't found twice.
            pattern(window_b, "task", r"\b(?:TRACK|WEB)-\d+\b"),
        ];
        let text = "Fix TRACK-142 and WEB-7, not UTF-8";
        let found = find_in(&patterns, Some(window_a), text);
        let texts: Vec<_> = found.iter().map(|link| link.text.as_str()).collect();
        assert_eq!(texts, vec!["TRACK-142", "WEB-7"]);
        assert_eq!(&text[found[0].range.clone()], "TRACK-142");
        assert_eq!(find_in(&patterns, None, text).len(), 2, "every window's, once");
        assert!(find_in(&patterns, Some(EntityId::from(9u64)), text).is_empty());

        // A plugin's links leave out what is clickable already.
        let taken = [Range { start: 0, end: 8 }];
        let targets = targets(find_in(&patterns, None, text), &taken);
        assert_eq!(targets.len(), 1);
        assert!(matches!(&targets[0].1, Target::Plugin(link) if link.text == "WEB-7"));
        // Without windows that set patterns, there are none.
        assert!(find(None, text).is_empty());
    }

    #[test]
    fn bare_web_addresses_end_where_the_sentence_goes_on() {
        let text = "See https://x.y/a_(b). And (http://c.d/e), or https://, https://f.g?h=1!";
        let found: Vec<_> = web_links(text).into_iter().map(|range| &text[range]).collect();
        assert_eq!(found, vec!["https://x.y/a_(b)", "http://c.d/e", "https://f.g?h=1"]);
        assert!(web_links("no links here").is_empty());
    }

    #[test]
    fn link_ranges_cut_the_runs() {
        let accent = gpui::red();
        // "Fix TRACK-142 now": a run of 4, then one of 13 holding the link at 4..13.
        let runs = link_runs(vec![run(4), run(13)], &[Range { start: 4, end: 13 }], accent);
        let lens: Vec<_> = runs.iter().map(|run| run.len).collect();
        assert_eq!(lens, vec![4, 9, 4]);
        assert_eq!(runs[1].color, accent);
        assert!(runs[1].underline.is_some());
        assert!(runs[0].underline.is_none() && runs[2].underline.is_none());

        // A link across two runs is cut in both.
        let runs = link_runs(vec![run(6), run(6)], &[Range { start: 3, end: 9 }], accent);
        let lens: Vec<_> = runs.iter().map(|run| run.len).collect();
        assert_eq!(lens, vec![3, 3, 3, 3]);
        let linked: Vec<_> = runs.iter().map(|run| run.underline.is_some()).collect();
        assert_eq!(linked, vec![false, true, true, false]);
        assert_eq!(runs.iter().map(|run| run.len).sum::<usize>(), 12);
    }
}
