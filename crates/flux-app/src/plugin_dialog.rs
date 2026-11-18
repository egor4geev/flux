//! A plugin's dialog (API 0.5, `location = "dialog"`): the plugin's view in a card in the middle
//! of the window, over everything — Settings too — as Flux's own dialogs are (the frame, the
//! backdrop, the motion of `Workspace::open_plugin_dialog`). A sign-in, a few steps of a setup.
//!
//! The header has the window's title and ×; under it the view, as tall as its content up to most
//! of the window, then the content scrolls. `ui.show` opens it and gives its first text field the
//! keyboard (else the dialog itself); `ui.hide`, Esc and × close it, and the keyboard goes back
//! where it was (the Settings page). The plugin hears `tool-window-shown` when it opens and
//! `tool-window-hidden` however it closes. One plugin dialog at a time in a window: showing
//! another closes the first. A dialog shown while a dialog or a popup of the user's is open waits
//! for it, as the plugins' forms do.
//!
//! Keys (context "PluginDialog"): Esc closes, ⇥ / ⇧⇥ go from field to field; ↵ in a field is the
//! view's `submitted`.

use gpui::{
    App, ClickEvent, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, KeyBinding, Render, WeakEntity, Window, actions, div, prelude::*, px, relative,
};

use crate::i18n::tr;
use crate::icons::IconName;
use crate::notifications_panel::{FocusEditor, Hide};
use crate::plugin_view::{FocusNextField, FocusPreviousField, PluginView};
use crate::plugins::{PendingQuestion, PluginStore, ToolKey};
use crate::theme::{self, Theme};
use crate::ui;
use crate::workspace::Workspace;

actions!(
    plugin_dialog,
    [
        /// Esc, ×: the dialog closes.
        Dismiss,
    ]
);

const CONTEXT: &str = "PluginDialog";
/// The dialog's width.
const WIDTH: f32 = 440.;
/// The share of the window's height the dialog grows to; then its content scrolls.
const MAX_SHARE: f32 = 0.85;

pub fn init(cx: &mut App) {
    let context = Some(CONTEXT);
    crate::keymap::bind_keys(
        cx,
        [
            KeyBinding::new("escape", Dismiss, context),
            // With the dialog itself focused (a view without fields): its first field, its last.
            KeyBinding::new("tab", FocusNextField, context),
            KeyBinding::new("shift-tab", FocusPreviousField, context),
        ],
    );
}

/// `ui.show` of a dialog window: it opens over Settings when they are open, as the window's
/// dialog otherwise; another plugin dialog shown gives way. With a dialog or a popup of the
/// user's open, it waits for it to close.
pub(crate) fn show(
    workspace: &mut Workspace,
    key: ToolKey,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(view) = workspace
        .plugins
        .read(cx)
        .tool_window(key)
        .map(|tool| tool.view.clone())
    else {
        return;
    };
    let open = shown(workspace, cx);
    if open == Some(key) {
        return;
    }
    match open {
        // One plugin dialog at a time: the open one closes (its plugin hears it).
        Some(open) => hide(workspace, open, window, cx),
        None if !workspace.can_ask_plugin_question() => {
            workspace.plugins.update(cx, |store, _| {
                store.forget_pending_dialog(key);
                store.queue_question(PendingQuestion::Dialog(key));
            });
            return;
        }
        None => {}
    }
    let plugins = workspace.plugins.downgrade();
    let shown_view = view.clone();
    workspace.open_plugin_dialog(window, cx, move |_, cx| {
        PluginDialog::new(key, plugins, shown_view, cx)
    });
    view.update(cx, |view, cx| view.set_visible(true, cx));
    workspace
        .plugins
        .update(cx, |store, _| store.tool_window_visibility(key, true));
}

/// `ui.hide` of a dialog window (or its plugin went away): it closes, or stops waiting.
pub(crate) fn hide(
    workspace: &mut Workspace,
    key: ToolKey,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if workspace
        .plugins
        .update(cx, |store, _| store.forget_pending_dialog(key))
    {
        return;
    }
    if workspace
        .over_view::<PluginDialog>()
        .is_some_and(|dialog| dialog.read(cx).key == key)
    {
        workspace.dismiss_over(window, cx);
    } else if workspace
        .modal_view::<PluginDialog>()
        .is_some_and(|dialog| dialog.read(cx).key == key)
    {
        workspace.dismiss_modal(window, cx);
    }
}

/// The window of the plugin dialog open in the window, if one is.
pub(crate) fn shown(workspace: &Workspace, cx: &App) -> Option<ToolKey> {
    workspace
        .over_view::<PluginDialog>()
        .or_else(|| workspace.modal_view::<PluginDialog>())
        .map(|dialog| dialog.read(cx).key)
}

/// A plugin's dialog window in its frame.
pub struct PluginDialog {
    key: ToolKey,
    plugins: WeakEntity<PluginStore>,
    view: Entity<PluginView>,
    /// The dialog's own focus: the view's fields are inside it, so the window's modal layer knows
    /// the keyboard is the dialog's whichever field has it.
    focus_handle: FocusHandle,
    /// Just opened: the first field takes the keyboard once the plugin's view is there (on a
    /// frame — focus events don't come to a window that isn't active).
    focus_first: bool,
    /// Closed: the plugin has heard it.
    closed: bool,
}

impl EventEmitter<DismissEvent> for PluginDialog {}

impl PluginDialog {
    fn new(
        key: ToolKey,
        plugins: WeakEntity<PluginStore>,
        view: Entity<PluginView>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            key,
            plugins,
            view,
            focus_handle: cx.focus_handle(),
            focus_first: true,
            closed: false,
        }
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    /// The window took the dialog away — Esc, ×, `ui.hide`, another dialog, Settings closing
    /// under it: the view is out of sight and the plugin hears it (`tool-window-hidden`), once.
    pub(crate) fn closed(&mut self, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.closed, true) {
            return;
        }
        self.view.update(cx, |view, cx| view.set_visible(false, cx));
        let key = self.key;
        self.plugins
            .update(cx, |store, _| store.tool_window_visibility(key, false))
            .ok();
    }

    fn step(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.view
            .update(cx, |view, cx| view.focus_field_step(forward, window, cx));
    }
}

impl Focusable for PluginDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PluginDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        if self.focus_first && self.focus_handle.is_focused(window) && self.view.read(cx).has_view()
        {
            self.focus_first = false;
            let view = self.view.clone();
            cx.defer_in(window, move |_, window, cx| {
                view.update(cx, |view, cx| view.focus_first_field(window, cx));
            });
        }
        let title = self.view.read(cx).title();
        let height = f32::from(window.viewport_size().height) * MAX_SHARE;
        let max_height = ui::dialog_height(height, window);
        ui::popover(ui)
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.dismiss(cx)))
            // Esc and ⇧Esc in the view: what hides a window of the islands closes the dialog.
            .on_action(cx.listener(|this, _: &FocusEditor, _, cx| this.dismiss(cx)))
            .on_action(cx.listener(|this, _: &Hide, _, cx| this.dismiss(cx)))
            .on_action(
                cx.listener(|this, _: &FocusNextField, window, cx| this.step(true, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &FocusPreviousField, window, cx| {
                    this.step(false, window, cx)
                }),
            )
            .w(px(WIDTH))
            .max_w(relative(0.9))
            .max_h(px(max_height))
            .flex()
            .flex_col()
            // Clicks on the dialog stay on it.
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pl_5()
                    .pr_3()
                    .pt_4()
                    .pb_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::TEXT_LG))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(
                        ui::icon_button("plugin-dialog-close", IconName::Close, ui)
                            .tooltip(ui::tooltip(tr("Close"), None))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.dismiss(cx))),
                    ),
            )
            .child(self.view.clone())
    }
}
