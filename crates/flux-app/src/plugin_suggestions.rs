//! Plugin suggestions (stage 8.3), as JetBrains IDEs suggest plugins: a file Flux has no language
//! for, while the catalog has a plugin with one, gets a banner above the editor — Install, or Ignore
//! for this kind of file. Once the plugin is installed its language takes the file, and the banner
//! goes by itself. It slides down from under the tabs as it comes and fades away as it goes
//! ([`Slot`]).
//!
//! The banner looks at the catalog the process already has ([`crate::plugin_catalog`]: the disk
//! cache, then what was read); without any, the catalog is read once in the background.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;

use flux_plugin::catalog::{self, IndexEntry};
use gpui::{
    AnyElement, App, ClickEvent, Context, Div, Entity, EntityId, SharedString, WeakEntity, Window,
    div, prelude::*, px,
};

use crate::editor::Editor;
use crate::i18n::{tr, trf};
use crate::icons::IconName;
use crate::theme::{self, Theme, UiColors};
use crate::workspace::Workspace;
use crate::{motion, plugin_catalog, settings, ui};

/// What the banner offers for a file.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub entry: IndexEntry,
    /// "*.rs" or the file name ("Dockerfile"): what «Ignore» remembers.
    pub kind: String,
    /// The language's name ("Rust").
    pub language: String,
}

/// The suggestion for a file: it has no language, the catalog has a compatible plugin for it, the
/// plugin isn't installed (an installed one turned off is the user's choice), and the kind of file
/// isn't ignored.
pub fn suggestion_for(
    path: &Path,
    workspace: &Workspace,
    cx: &mut Context<Workspace>,
) -> Option<Suggestion> {
    if flux_syntax::language_for_path(path).is_some() {
        return None;
    }
    let Some(index) = plugin_catalog::index(cx) else {
        plugin_catalog::ensure_fresh(cx);
        return None;
    };
    let entry = catalog::suggest(&index, path)?;
    if workspace.plugins.read(cx).plugin(&entry.id).is_some() {
        return None;
    }
    let kind = catalog::suggestion_kind(entry, path)?;
    if settings::suggestion_ignored(&kind, cx) {
        return None;
    }
    let language = entry
        .language_for(path)
        .map_or_else(|| entry.name.clone(), |language| language.name.clone());
    Some(Suggestion {
        entry: entry.clone(),
        kind,
        language,
    })
}

/// The banner above `editor`, if its file has no language and the catalog has one for it — or one
/// that has just been dismissed, on its way out.
pub fn banner(
    workspace: &Workspace,
    editor: &Entity<Editor>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Option<AnyElement> {
    let suggestion = editor
        .read(cx)
        .document
        .path()
        .map(Path::to_path_buf)
        .and_then(|path| suggestion_for(&path, workspace, cx));
    // A scenario can't click the banner: `FLUX_SCENARIO_ACCEPT_SUGGESTION=1` installs the first
    // suggestion it shows.
    #[cfg(feature = "scenario")]
    if let Some(suggestion) = &suggestion {
        use std::sync::atomic::{AtomicBool, Ordering};
        static ACCEPTED: AtomicBool = AtomicBool::new(false);
        if std::env::var_os("FLUX_SCENARIO_ACCEPT_SUGGESTION").is_some()
            && !ACCEPTED.swap(true, Ordering::Relaxed)
        {
            let entry = suggestion.entry.clone();
            cx.defer_in(window, move |_, window, cx| {
                plugin_catalog::install(entry, cx.weak_entity(), window, cx)
            });
        }
    }
    let content = suggestion.map(|suggestion| Content::new(suggestion, cx));
    let workspace = cx.weak_entity();
    let frame = SLOTS.with_borrow_mut(|slots| {
        slots.retain(|slot| slot.workspace.upgrade().is_some());
        let slot = match slots
            .iter()
            .position(|slot| slot.workspace.entity_id() == workspace.entity_id())
        {
            Some(index) => &mut slots[index],
            None => {
                slots.push(Slot::new(workspace));
                slots.last_mut().expect("just pushed")
            }
        };
        slot.update(editor.entity_id(), content);
        slot.frame(window)
    })?;
    Some(draw(frame, cx))
}

/// What the banner says, as it was last drawn: kept for its exit (an installed plugin's banner
/// leaves with its last progress).
#[derive(Clone)]
struct Content {
    suggestion: Suggestion,
    text: String,
    links: Links,
}

/// The links at the end of the banner.
#[derive(Clone, Copy)]
enum Links {
    /// Install, or ignore this kind of file.
    Offer,
    /// Downloading: it can be cancelled.
    Cancel,
    /// Installing: nothing to press.
    Busy,
}

impl Content {
    fn new(suggestion: Suggestion, cx: &App) -> Self {
        let progress = plugin_catalog::progress(&suggestion.entry.id, cx);
        let text = match &progress {
            Some(progress) => trf(
                "Installing “{0}”: {1}",
                &[
                    &suggestion
                        .entry
                        .translate(crate::i18n::lang_code(), &suggestion.entry.name),
                    &progress.label(),
                ],
            ),
            None => trf("Plugins supporting {0} files found.", &[&suggestion.kind]),
        };
        let links = match progress {
            Some(progress) if !progress.installing() => Links::Cancel,
            Some(_) => Links::Busy,
            None => Links::Offer,
        };
        Self {
            suggestion,
            text,
            links,
        }
    }
}

thread_local! {
    /// The banner of each window, as it comes and goes ([`Slot`]).
    static SLOTS: RefCell<Vec<Slot>> = const { RefCell::new(Vec::new()) };
}

/// The banner of a window's editor area as it comes and goes (`crate::motion`): it slides down
/// from under the tabs when it comes to the editor in sight (the catalog read meanwhile, the
/// plugin removed), as the find bar does; dismissed — Ignore, the plugin installed — it fades out
/// taking no input while the room it took closes. Another editor in sight (a tab switch, a file
/// opened) shows its banner at once, as the editor itself shows. The suggestion is gone by the
/// time the banner leaves, so its state lives here, one per window (its workspace).
struct Slot {
    workspace: WeakEntity<Workspace>,
    /// The editor whose banner the slot shows.
    editor: Option<EntityId>,
    /// What the banner shows; `None` once it has gone.
    content: Option<Content>,
    /// The banner that went, fading out.
    leaving: Option<motion::Leaving<Content>>,
    /// How much of the banner's room shows, from and to, since when ([`motion::SMOOTH`]).
    reveal: (f32, f32, Instant),
    /// The banner's height as last laid out, the gap above it included: how far it slides (the
    /// first slide starts from the frame that measured it).
    height: Rc<Cell<f32>>,
}

/// What a slot draws in a frame.
struct Frame {
    content: Content,
    /// Gone: drawn without input, fading out — how far its exit has come (0 → 1).
    leaving: Option<f32>,
    /// Sliding: how much of the room shows; `None` — at rest, laid out as it is.
    shown: Option<f32>,
    height: Rc<Cell<f32>>,
}

impl Slot {
    fn new(workspace: WeakEntity<Workspace>) -> Self {
        Self {
            workspace,
            editor: None,
            content: None,
            leaving: None,
            reveal: (0., 0., Instant::now()),
            height: Rc::new(Cell::new(0.)),
        }
    }

    /// How far the slide has come (0 → 1) and how much of the room shows.
    fn revealed(&self) -> (f32, f32) {
        let (from, to, since) = self.reveal;
        if from == to {
            // At rest (another editor's banner is simply there).
            return (1., to);
        }
        let (t, _) = motion::SMOOTH.at(since.elapsed());
        (t, motion::lerp(from, to, t))
    }

    /// The room opens (`open`) or closes, from where it is.
    fn slide(&mut self, open: bool) {
        let to = if open { 1. } else { 0. };
        if self.reveal.1 != to {
            let (_, shown) = self.revealed();
            self.reveal = (shown, to, Instant::now());
        }
    }

    /// `editor` is in sight, and its banner says `content` (`None` — no suggestion).
    fn update(&mut self, editor: EntityId, content: Option<Content>) {
        if self.editor != Some(editor) {
            // Another editor: its banner is there at once, as the editor is.
            let shown = if content.is_some() { 1. } else { 0. };
            self.editor = Some(editor);
            self.content = content;
            self.leaving = None;
            self.reveal = (shown, shown, Instant::now());
            return;
        }
        match content {
            Some(content) => {
                self.content = Some(content);
                self.leaving = None;
                self.slide(true);
            }
            None => {
                if let Some(content) = self.content.take() {
                    self.leaving = Some(motion::Leaving::new(content));
                    self.slide(false);
                }
            }
        }
    }

    /// What to draw now, asking for the next frame while the banner moves.
    fn frame(&mut self, window: &Window) -> Option<Frame> {
        let (t, shown) = self.revealed();
        let (content, leaving) = match (&self.content, &self.leaving) {
            (Some(content), _) => (content.clone(), None),
            (None, Some(leaving)) if t < 1. => {
                (leaving.value.clone(), Some(leaving.progress(window)))
            }
            _ => {
                self.leaving = None;
                return None;
            }
        };
        if t < 1. {
            window.request_animation_frame();
        }
        Some(Frame {
            content,
            leaving,
            shown: (t < 1.).then_some(shown),
            height: self.height.clone(),
        })
    }
}

/// The banner of `frame`: at rest as it is; sliding, in a room that grows or shrinks, the banner
/// coming down from under the tabs within it (fading in over the first half) or going back up,
/// fading out without input.
fn draw(frame: Frame, cx: &mut Context<Workspace>) -> AnyElement {
    let measured = frame.height.clone();
    let banner = div()
        .flex_none()
        .relative()
        .px_2()
        .pt_1p5()
        .child(render(&frame.content, cx))
        // Its height, for the slide.
        .child(
            gpui::canvas(
                move |bounds, window, _| {
                    let height = f32::from(bounds.size.height);
                    if measured.replace(height) != height {
                        window.request_animation_frame();
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        );
    let Some(shown) = frame.shown else {
        return banner.into_any_element();
    };
    let height = frame.height.get();
    let banner = match frame.leaving {
        None => banner.opacity((shown / 0.5).clamp(0., 1.)),
        Some(progress) => motion::leaving_style(banner, progress, (0.5, 0.)),
    }
    .transform_translate(px(0.), px(-height * (1. - shown)));
    let room = div()
        .flex_none()
        .flex()
        .flex_col()
        .overflow_hidden()
        .h(px(height * shown))
        .child(banner);
    match frame.leaving {
        None => room.into_any_element(),
        Some(_) => motion::inert(room).into_any_element(),
    }
}

/// The banner itself: the puzzle, what it says, its links.
fn render(content: &Content, cx: &mut Context<Workspace>) -> Div {
    let ui = Theme::ui(cx);
    let suggestion = &content.suggestion;
    let mut links: Vec<AnyElement> = Vec::new();
    match content.links {
        Links::Cancel => {
            let id = suggestion.entry.id.clone();
            links.push(
                link("plugin-suggestion-cancel", tr("Cancel"), ui)
                    .on_click(move |_, _, cx| plugin_catalog::cancel(&id, cx))
                    .into_any_element(),
            );
        }
        Links::Busy => {}
        Links::Offer => {
            let entry = suggestion.entry.clone();
            links.push(
                link(
                    "plugin-suggestion-install",
                    trf("Install {0}", &[&suggestion.language]),
                    ui,
                )
                .on_click(cx.listener(move |_, _: &ClickEvent, window, cx| {
                    plugin_catalog::install(entry.clone(), cx.weak_entity(), window, cx)
                }))
                .into_any_element(),
            );
            let kind = suggestion.kind.clone();
            let label = if kind.starts_with("*.") {
                tr("Ignore Extension")
            } else {
                tr("Ignore")
            };
            links.push(
                link("plugin-suggestion-ignore", label, ui)
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        settings::ignore_suggestion(&kind, cx);
                        cx.notify()
                    }))
                    .into_any_element(),
            );
        }
    }
    div()
        .px_3()
        .py_1p5()
        .flex()
        .items_center()
        .gap_2p5()
        .rounded(px(ui::RADIUS_SM))
        .bg(UiColors::tint(ui.accent, 0.10))
        .border_1()
        .border_color(UiColors::tint(ui.accent, 0.22))
        .text_size(px(theme::TEXT_SM))
        .child(
            crate::icons::icon(IconName::Puzzle, ui.accent_text)
                .size(px(14.))
                .flex_none(),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(ui.foreground)
                .child(content.text.clone()),
        )
        .children(links)
}

/// A link of the banner.
fn link(
    id: &'static str,
    label: impl Into<SharedString>,
    ui: UiColors,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .text_color(ui.accent_text)
        .font_weight(gpui::FontWeight::MEDIUM)
        .cursor_pointer()
        .hover(|style| style.underline())
        .child(label.into())
}
