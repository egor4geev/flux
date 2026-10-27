//! Settings → Editor (part 10.5): soft wraps — the files whose long lines wrap, the indent of the
//! wrapped parts, the wrap signs (JetBrains IDEs: Editor → General → Soft Wraps). A change applies
//! to the open editors at once; an editor switched with ⌃⌥W keeps its own choice.

use gpui::{
    App, ClickEvent, Context, Div, FontWeight, IntoElement, Render, SharedString, Subscription,
    Window, div, prelude::*, px,
};

use crate::i18n::tr;
use crate::input::{InputEvent, TextInput};
use crate::settings::{self, EditorSettings};
use crate::theme::{self, Theme, UiColors};
use crate::ui;
use crate::wrap::{self, ToggleSoftWrap};

pub fn init(_cx: &mut App) {}

/// The additional indents offered, in spaces.
const EXTRA_INDENTS: [u32; 4] = [0, 2, 4, 8];

/// The page, made when Settings first show it.
pub struct EditorSettingsPage {
    /// The masks of the files that wrap, as typed: "*.md; *.txt".
    masks: gpui::Entity<TextInput>,
    /// Where the extra indent's steps were drawn: the selected one's pill glides between them.
    indent_steps: std::rc::Rc<ui::Segments>,
    _subscription: Subscription,
}

impl EditorSettingsPage {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let text = wrap::masks_text(&settings::editor(cx).soft_wrap_files);
        let masks = cx.new(|cx| {
            let mut input = TextInput::new("*.md; *.txt", cx).code();
            input.set_text(&text, cx);
            input
        });
        let subscription = cx.subscribe(&masks, |_, input, _: &InputEvent, cx| {
            let masks = wrap::parse_masks(&input.read(cx).text());
            change(cx, |editor| editor.soft_wrap_files = masks);
        });
        Self {
            masks,
            indent_steps: Default::default(),
            _subscription: subscription,
        }
    }
}

/// Changes the editor settings, saves them, and redraws the windows: their editors wrap as the
/// settings say now.
fn change(cx: &mut App, change: impl FnOnce(&mut EditorSettings)) {
    settings::update_editor(cx, change);
    cx.refresh_windows();
}

/// A row with a title, a line of detail under it, and a switch on the right; a click anywhere on
/// it switches.
fn switch_row(
    id: &'static str,
    title: &'static str,
    detail: &'static str,
    on: bool,
    ui: UiColors,
) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .py_2()
        .rounded(px(ui::RADIUS_MD))
        .cursor_pointer()
        .hover(move |style| style.bg(ui.hover))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(title)
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(ui.dim)
                        .child(detail),
                ),
        )
        .child(ui::switch(
            SharedString::from(format!("{id}-switch")),
            on,
            ui,
        ))
}

impl Render for EditorSettingsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let current = settings::editor(cx);
        let indent = current.soft_wrap_indent;
        let current_line_only = current.soft_wrap_signs_current_line_only;
        let files = div()
            .flex()
            .flex_col()
            .gap_1p5()
            .px_3()
            .py_2()
            .child(tr("Soft-wrap these files:"))
            .child(self.masks.clone())
            .child(
                div()
                    .text_size(px(theme::TEXT_SM))
                    .text_color(ui.dim)
                    .child(tr(
                        "Masks of file names, separated with semicolons: *.md, README.*. Other files keep long lines whole, with a horizontal scroll.",
                    )),
            );
        let extra = current.soft_wrap_extra_indent;
        let selected_step = EXTRA_INDENTS.iter().position(|&spaces| spaces == extra);
        let pill = self.indent_steps.pill(selected_step, "indent-pill", ui);
        let own_backdrop = pill.is_none();
        let steps = EXTRA_INDENTS.iter().enumerate().map(|(index, &spaces)| {
            let selected = extra == spaces;
            div()
                .id(("soft-wrap-indent", index))
                .flex_none()
                .min_w(px(40.))
                .h(px(26.))
                .px_2()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(ui::RADIUS_SM))
                .text_color(if selected {
                    ui.accent_text
                } else {
                    ui.text_muted
                })
                .when(selected, |chip| chip.font_weight(FontWeight::SEMIBOLD))
                .when(selected && own_backdrop, |chip| chip.bg(ui.list_selected))
                .when(indent && !selected, |chip| {
                    chip.cursor_pointer()
                        .hover(move |style| style.bg(ui.hover).text_color(ui.foreground))
                })
                .when(indent, |chip| {
                    chip.on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        change(cx, |editor| editor.soft_wrap_extra_indent = spaces);
                        cx.notify();
                    }))
                })
                .child(self.indent_steps.measure(index))
                .child(spaces.to_string())
        });
        let extra_row = div()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_1p5()
            .when(!indent, |row| row.opacity(0.5))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(ui.text_muted)
                    .child(tr("Add additional indent, in spaces:")),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .gap_1()
                    .p_1()
                    .rounded(px(ui::RADIUS_MD))
                    .border_1()
                    .border_color(ui.island_border)
                    .child(self.indent_steps.measure_origin())
                    .children(pill)
                    .children(steps),
            );
        let card = div()
            .flex()
            .flex_col()
            .gap_0p5()
            .p_1()
            .rounded(px(ui::RADIUS_MD))
            .border_1()
            .border_color(ui.island_border)
            .child(
                div()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .child(ui::section_label(tr("Soft Wraps"), ui)),
            )
            .child(files)
            .child(div().mx_3().my_1().child(ui::divider(ui)))
            .child(
                switch_row(
                    "soft-wrap-indent",
                    tr("Use original line’s indent for wrapped fragments"),
                    tr("The wrapped part of a line starts under the line’s text, not at the edge."),
                    indent,
                    ui,
                )
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    change(cx, |editor| editor.soft_wrap_indent = !indent);
                    cx.notify();
                })),
            )
            .child(extra_row)
            .child(
                switch_row(
                    "soft-wrap-signs",
                    tr("Only show soft-wrap indicators for the current line"),
                    tr("⤶ marks where a line wraps and ⤷ where it goes on. Off: on every wrapped line."),
                    current_line_only,
                    ui,
                )
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    change(cx, |editor| {
                        editor.soft_wrap_signs_current_line_only = !current_line_only
                    });
                    cx.notify();
                })),
            );
        // ⌃⌥W, as the command has it.
        let hint = ui::shortcut_for(&ToggleSoftWrap, cx).map(|keys| {
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(px(theme::TEXT_SM))
                .child(ui::keys(&keys, ui))
                .child(div().text_color(ui.dim).child(tr(
                    "Soft-Wrap: on or off in the editor you are in, whatever the file",
                )))
        });
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(theme::TEXT_LG))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(tr("Editor")),
                    )
                    .child(div().text_color(ui.text_muted).child(tr(
                        "Soft wraps: a long line goes on in the next rows instead of past the edge of the editor. A commit message always wraps.",
                    ))),
            )
            .child(card)
            .children(hint)
    }
}
