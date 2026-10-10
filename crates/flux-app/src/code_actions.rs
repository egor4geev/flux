//! ⌥↵ — Show Context Actions (part 9.2), as in JetBrains IDEs: a popup at the caret with what the
//! language servers offer there (`textDocument/codeAction`: quick fixes of the problems first,
//! then refactorings and source actions) and, at the bottom, Claude (Fix / Explain with Claude,
//! [`crate::claude_actions`]). A chosen action applies its edit to the open documents and the files
//! on disk (one undo step per document) or runs its command on the server.
//!
//! - Every server of the document is asked (Python: pyright and ruff), with the problems under
//!   the caret or the selection as the context — as the server published them, `data` included
//!   (ruff carries its fixes there).
//! - The popup is a window popup at the caret (`Workspace::toggle_modal` + `anchor_modal`): ↑↓,
//!   ↵, Esc, the mouse; typing filters the list (speed search, as in JetBrains); actions a server
//!   offers but can't apply here are dimmed with its reason.
//! - Applying: `codeAction/resolve` when the edit is left out and the server resolves; then the
//!   workspace edit ([`apply_edit`]: open documents through their editors, other files on disk,
//!   files created, renamed and deleted in order); then the command (`workspace/executeCommand`) —
//!   during which the server may send its own edits (`workspace/applyEdit`, [`apply_server_edit`]).

use std::path::{Path, PathBuf};

use flux_lsp::edit::FileChange;
use flux_lsp::lsp_types::notification::DidChangeWatchedFiles;
use flux_lsp::lsp_types::request::{CodeActionRequest, CodeActionResolveRequest, ExecuteCommand};
use flux_lsp::lsp_types::{
    self, CodeAction, CodeActionContext, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, CodeActionTriggerKind, Command, DidChangeWatchedFilesParams,
    ExecuteCommandParams, FileChangeType, FileEvent, NumberOrString, ServerCapabilities,
    TextDocumentIdentifier, WorkspaceEdit,
};
use flux_lsp::{ApplyEdit, LanguageServer, position};
use gpui::{
    Action, AnyElement, App, ClickEvent, Context, DismissEvent, Div, Entity, EventEmitter,
    FocusHandle, Focusable, KeyBinding, KeyDownEvent, Render, SharedString, Task, WeakEntity,
    Window, actions, div, prelude::*, px,
};

use crate::diagnostics::{Diagnostic, Severity};
use crate::editor::Editor;
use crate::i18n::{tr, trf};
use crate::icons::{IconName, icon};
use crate::navigation::canonical;
use crate::notification_center::NotificationGroup;
use crate::notifications::Notification;
use crate::popup;
use crate::theme::{self, Theme};
use crate::ui::{self, RADIUS_SM};
use crate::workspace::Workspace;

actions!(
    code_actions,
    [
        /// ⌥↵: the context actions at the caret.
        ShowContextActions,
    ]
);

// The popup's keys (context "ContextActions").
actions!(context_actions, [SelectNext, SelectPrevious, Confirm, Dismiss]);

const ROW_HEIGHT: f32 = 28.;
const MIN_WIDTH: f32 = 280.;
const MAX_WIDTH: f32 = 560.;
const MAX_VISIBLE_ROWS: usize = 14;
/// Inset of the rows from the popup edge, as the context menu's.
const PADDING: f32 = 5.;

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "alt-enter",
        ShowContextActions,
        Some("Editor"),
    )]);
    let popup = Some("ContextActions");
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, popup),
        KeyBinding::new("up", SelectPrevious, popup),
        KeyBinding::new("ctrl-n", SelectNext, popup),
        KeyBinding::new("ctrl-p", SelectPrevious, popup),
        KeyBinding::new("enter", Confirm, popup),
        KeyBinding::new("escape", Dismiss, popup),
    ]);
}

/// The editor's handlers: none — ⌥↵ goes up to the window ([`workspace_actions`]), where the popup
/// lives and the edits of other files are applied.
pub fn actions(root: Div, _editor: &Editor, _cx: &mut Context<Editor>) -> Div {
    root
}

/// The popup is the window's (a modal popup at the caret), not the editor's.
pub fn render(
    _editor: &Editor,
    _window: &mut Window,
    _cx: &mut Context<Editor>,
) -> Option<AnyElement> {
    None
}

/// The window's handlers: ⌥↵ from a focused editor.
pub fn workspace_actions(root: Div, cx: &mut Context<Workspace>) -> Div {
    root.on_action(cx.listener(|workspace, _: &ShowContextActions, window, cx| {
        show(workspace, window, cx)
    }))
}

// --- The rows ---

/// Where a row goes in the popup: quick fixes first (the server's preferred one on top), then
/// refactorings, source actions, the rest; Claude at the bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Group {
    PreferredFix,
    QuickFix,
    Refactor,
    Source,
    Other,
    Claude,
}

impl Group {
    /// The group of a code action by its kind (`quickfix`, `refactor.extract`, `source.fixAll`…).
    pub(crate) fn of(kind: Option<&CodeActionKind>, preferred: bool) -> Group {
        let kind = kind.map(|kind| kind.as_str()).unwrap_or_default();
        let is = |base: &str| kind == base || kind.starts_with(&format!("{base}."));
        if is("quickfix") {
            if preferred {
                Group::PreferredFix
            } else {
                Group::QuickFix
            }
        } else if is("refactor") {
            Group::Refactor
        } else if is("source") {
            Group::Source
        } else if preferred {
            // No kind, but preferred: a fix in all but name.
            Group::QuickFix
        } else {
            Group::Other
        }
    }
}

enum RowAction {
    /// A server's code action or command.
    Lsp {
        server: LanguageServer,
        item: Box<CodeActionOrCommand>,
    },
    /// A window action (Fix / Explain with Claude).
    Dispatch(Box<dyn Action>),
}

struct Row {
    title: SharedString,
    group: Group,
    /// Offered but not applicable here, and why.
    disabled: Option<SharedString>,
    action: RowAction,
}

/// The rows of one server's answer, in its order.
fn rows_of(server: &LanguageServer, items: Vec<CodeActionOrCommand>) -> Vec<Row> {
    items
        .into_iter()
        .map(|item| {
            let (title, group, disabled) = match &item {
                CodeActionOrCommand::Command(command) => {
                    (command.title.clone(), Group::Other, None)
                }
                CodeActionOrCommand::CodeAction(action) => (
                    action.title.clone(),
                    Group::of(action.kind.as_ref(), action.is_preferred == Some(true)),
                    action
                        .disabled
                        .as_ref()
                        .map(|disabled| SharedString::from(disabled.reason.clone())),
                ),
            };
            Row {
                title: title.into(),
                group,
                disabled,
                action: RowAction::Lsp {
                    server: server.clone(),
                    item: Box::new(item),
                },
            }
        })
        .collect()
}

/// Rows by group, the servers' order within a group (a stable sort); disabled ones after the
/// enabled ones of their group.
fn sort_rows(rows: &mut [Row]) {
    rows.sort_by(|a, b| {
        a.group
            .cmp(&b.group)
            .then_with(|| a.disabled.is_some().cmp(&b.disabled.is_some()))
    });
}

/// Speed search: the rows whose title has the query's characters in order (case-insensitive).
pub(crate) fn matches(title: &str, query: &str) -> bool {
    let mut title = title.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|c| !c.is_whitespace())
        .all(|wanted| title.any(|c| c == wanted))
}

// --- Asking the servers ---

/// The servers of the document that offer code actions, each with its id.
fn code_action_servers(editor: &Editor) -> Vec<(u64, LanguageServer, lsp_types::Uri)> {
    editor
        .lsp
        .iter()
        .filter(|doc| {
            doc.server
                .capabilities()
                .is_some_and(|capabilities| offers_code_actions(&capabilities))
        })
        .map(|doc| (doc.server_id, doc.server.clone(), doc.uri.clone()))
        .collect()
}

fn offers_code_actions(capabilities: &ServerCapabilities) -> bool {
    match &capabilities.code_action_provider {
        Some(CodeActionProviderCapability::Simple(offers)) => *offers,
        Some(CodeActionProviderCapability::Options(_)) => true,
        None => false,
    }
}

fn resolves_code_actions(capabilities: &ServerCapabilities) -> bool {
    matches!(
        &capabilities.code_action_provider,
        Some(CodeActionProviderCapability::Options(options)) if options.resolve_provider == Some(true)
    )
}

/// The problems a code action request carries for server `owner`: those of the editor over
/// `range` (the caret's when it is empty), as the server published them when they can be found
/// (with `data`), with their current ranges.
fn context_diagnostics(
    editor: &Editor,
    owner: u64,
    published: Option<&[lsp_types::Diagnostic]>,
    range: std::ops::Range<usize>,
) -> Vec<lsp_types::Diagnostic> {
    let text = editor.document.text();
    overlapping(editor.diagnostics.iter(), range)
        .filter(|diagnostic| diagnostic.owner == owner)
        .map(|diagnostic| {
            let current = position::range_to_lsp(text, diagnostic.range.clone());
            let original = published.and_then(|published| {
                published.iter().find(|raw| same_problem(raw, diagnostic))
            });
            match original {
                Some(raw) => lsp_types::Diagnostic {
                    range: current,
                    ..raw.clone()
                },
                None => lsp_types::Diagnostic {
                    range: current,
                    severity: Some(match diagnostic.severity {
                        Severity::Error => lsp_types::DiagnosticSeverity::ERROR,
                        Severity::Warning => lsp_types::DiagnosticSeverity::WARNING,
                        Severity::Info => lsp_types::DiagnosticSeverity::INFORMATION,
                        Severity::Hint => lsp_types::DiagnosticSeverity::HINT,
                    }),
                    code: diagnostic.code.clone().map(NumberOrString::String),
                    source: diagnostic.source.clone(),
                    message: diagnostic.message.clone(),
                    ..Default::default()
                },
            }
        })
        .collect()
}

/// The diagnostics over a range: touching the caret when it is empty.
pub(crate) fn overlapping<'a>(
    diagnostics: impl Iterator<Item = &'a Diagnostic>,
    range: std::ops::Range<usize>,
) -> impl Iterator<Item = &'a Diagnostic> {
    diagnostics.filter(move |diagnostic| {
        let d = &diagnostic.range;
        if range.is_empty() {
            (d.start <= range.start && range.start <= d.end) || d.start == range.start
        } else {
            d.start < range.end && range.start < d.end.max(d.start + 1)
        }
    })
}

/// A published diagnostic is the editor's (same message and severity, code and source).
fn same_problem(raw: &lsp_types::Diagnostic, diagnostic: &Diagnostic) -> bool {
    let code = raw.code.as_ref().map(|code| match code {
        NumberOrString::Number(n) => n.to_string(),
        NumberOrString::String(s) => s.clone(),
    });
    raw.message == diagnostic.message && code == diagnostic.code && raw.source == diagnostic.source
}

/// ⌥↵: asks the servers of the focused document and shows the popup at the caret.
fn show(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let editor = workspace
        .editors(cx)
        .into_iter()
        .find(|editor| editor.read(cx).focus_handle.is_focused(window))
        .or_else(|| workspace.active_editor());
    let Some(editor) = editor else {
        return;
    };
    let (requests, has_problems, path) = {
        let state = editor.read(cx);
        if state.read_only {
            return;
        }
        let selection = state.document.selection().primary();
        let range = selection.from()..selection.to();
        let text = state.document.text();
        let lsp_range = position::range_to_lsp(text, range.clone());
        let path = state.document.path().map(Path::to_path_buf);
        let lsp = workspace.lsp.read(cx);
        let requests: Vec<_> = code_action_servers(state)
            .into_iter()
            .map(|(owner, server, uri)| {
                let published = path
                    .as_deref()
                    .and_then(|path| lsp.published(owner, path));
                let diagnostics = context_diagnostics(state, owner, published, range.clone());
                let params = CodeActionParams {
                    text_document: TextDocumentIdentifier { uri },
                    range: lsp_range,
                    context: CodeActionContext {
                        diagnostics,
                        only: None,
                        trigger_kind: Some(CodeActionTriggerKind::INVOKED),
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                };
                let response = server.request::<CodeActionRequest>(params);
                (server, response)
            })
            .collect();
        let has_problems = overlapping(state.diagnostics.iter(), range)
            .any(|diagnostic| diagnostic.severity <= Severity::Warning);
        (requests, has_problems, path)
    };
    let waiting = if requests.is_empty() && path.is_some() {
        workspace.lsp.read(cx).waiting(editor.entity_id())
    } else {
        None
    };
    let mut claude_rows = Vec::new();
    if crate::claude::enabled(cx) {
        if has_problems {
            claude_rows.push(Row {
                title: tr("Fix with Claude").into(),
                group: Group::Claude,
                disabled: None,
                action: RowAction::Dispatch(Box::new(crate::claude_actions::FixWithClaude)),
            });
        }
        claude_rows.push(Row {
            title: tr("Explain with Claude").into(),
            group: Group::Claude,
            disabled: None,
            action: RowAction::Dispatch(Box::new(crate::claude_actions::ExplainWithClaude)),
        });
    }
    let anchor = popup::caret_point(workspace, window, cx);
    let this = cx.entity().downgrade();
    let pending = requests.len();
    workspace.toggle_modal::<ContextActions>(window, cx, move |window, cx| {
        let mut popup = ContextActions {
            focus_handle: cx.focus_handle(),
            previous_focus: window.focused(cx),
            workspace: this,
            rows: claude_rows,
            pending,
            waiting: waiting.map(SharedString::from),
            query: String::new(),
            selected: 0,
            _requests: Vec::new(),
        };
        for (server, response) in requests {
            popup._requests.push(cx.spawn(
                async move |popup: WeakEntity<ContextActions>, cx: &mut gpui::AsyncApp| {
                let items = match response.await {
                    Ok(items) => items.unwrap_or_default(),
                    Err(error) => {
                        if !error.is_outdated() {
                            eprintln!("flux: code actions of {}: {error}", server.name());
                        }
                        Vec::new()
                    }
                };
                popup
                    .update(cx, |popup, cx| popup.add(rows_of(&server, items), cx))
                    .ok();
                },
            ));
        }
        popup
    });
    if let Some(anchor) = anchor {
        workspace.anchor_modal(anchor);
    }
}

// --- The popup ---

pub(crate) struct ContextActions {
    focus_handle: FocusHandle,
    /// The editor: the chosen action is applied with the keyboard back there.
    previous_focus: Option<FocusHandle>,
    workspace: WeakEntity<Workspace>,
    rows: Vec<Row>,
    /// Servers still answering.
    pending: usize,
    /// Why the document has no server to ask yet.
    waiting: Option<SharedString>,
    /// Speed search.
    query: String,
    /// Among the rows the query lets through.
    selected: usize,
    _requests: Vec<Task<()>>,
}

impl EventEmitter<DismissEvent> for ContextActions {}

impl Focusable for ContextActions {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ContextActions {
    /// A server answered: its rows join, in their group; the selection stays on the first row
    /// until the user moves it.
    fn add(&mut self, rows: Vec<Row>, cx: &mut Context<Self>) {
        self.pending = self.pending.saturating_sub(1);
        self.rows.extend(rows);
        sort_rows(&mut self.rows);
        if self.selected != 0 {
            self.selected = self.selected.min(self.visible().len().saturating_sub(1));
        }
        cx.notify();
    }

    /// The rows the query lets through, by index.
    fn visible(&self) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| matches(&row.title, &self.query))
            .map(|(index, _)| index)
            .collect()
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.visible().len();
        if count > 0 {
            self.selected = (self.selected + 1) % count;
            cx.notify();
        }
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.visible().len();
        if count > 0 {
            self.selected = (self.selected + count - 1) % count;
            cx.notify();
        }
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(&index) = self.visible().get(self.selected) {
            self.run(index, window, cx);
        }
    }

    /// Speed search: printable keys narrow the list, Backspace widens it.
    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = &keystroke.modifiers;
        if modifiers.control || modifiers.platform || modifiers.function {
            return;
        }
        if keystroke.key == "backspace" {
            if self.query.pop().is_some() {
                self.selected = 0;
                cx.notify();
            }
            cx.stop_propagation();
            return;
        }
        let Some(typed) = keystroke.key_char.as_deref() else {
            return;
        };
        if typed.chars().any(char::is_control) || typed.is_empty() {
            return;
        }
        self.query.push_str(typed);
        self.selected = 0;
        cx.stop_propagation();
        cx.notify();
    }

    /// Applies a row: the popup closes, the keyboard goes back to the editor, then the action runs.
    fn run(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        if let Some(reason) = &row.disabled {
            let message = trf("Not available: {0}", &[reason]);
            self.workspace
                .update(cx, |workspace, cx| workspace.show_message(message.into(), cx))
                .ok();
            return;
        }
        if let Some(focus) = &self.previous_focus {
            window.focus(focus);
        }
        cx.emit(DismissEvent);
        match &row.action {
            RowAction::Dispatch(action) => window.dispatch_action(action.boxed_clone(), cx),
            RowAction::Lsp { server, item } => {
                let (server, item) = (server.clone(), (**item).clone());
                self.workspace
                    .update(cx, |workspace, cx| run_action(workspace, server, item, cx))
                    .ok();
            }
        }
    }
}

impl Render for ContextActions {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Theme::ui(cx);
        let visible = self.visible();
        let selected = self.selected.min(visible.len().saturating_sub(1));
        let mut items: Vec<AnyElement> = Vec::new();
        let mut previous_group: Option<Group> = None;
        for (position, &index) in visible.iter().enumerate() {
            let row = &self.rows[index];
            // A thin line between Claude and the servers' actions, as JetBrains separates groups.
            if previous_group.is_some_and(|group| {
                (group == Group::Claude) != (row.group == Group::Claude)
            }) {
                items.push(ui::divider(ui).mx_1p5().my_1().into_any_element());
            }
            previous_group = Some(row.group);
            let (glyph, color) = match row.group {
                Group::PreferredFix | Group::QuickFix => (IconName::Bulb, ui.error),
                Group::Claude => (IconName::Claude, ui.accent_text),
                _ => (IconName::Bulb, ui.warning),
            };
            let enabled = row.disabled.is_none();
            let is_selected = position == selected;
            items.push(
                div()
                    .id(index)
                    .h(px(ROW_HEIGHT))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(RADIUS_SM))
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .text_color(if enabled { ui.foreground } else { ui.text_disabled })
                    .when(is_selected, |row| row.bg(ui.list_selected))
                    .when(!is_selected, |row| row.hover(|style| style.bg(ui.hover)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.run(index, window, cx)
                    }))
                    .child(
                        icon(glyph, if enabled { color } else { ui.text_disabled })
                            .flex_none()
                            .size(px(14.)),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(row.title.clone()))
                    .children(row.disabled.clone().map(|reason| {
                        div()
                            .flex_none()
                            .max_w(px(220.))
                            .truncate()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(ui.dim)
                            .child(reason)
                    }))
                    .into_any_element(),
            );
        }
        let status: Option<SharedString> = if self.pending > 0 {
            Some(tr("Loading…").into())
        } else if !self.rows.iter().any(|row| row.group != Group::Claude) {
            Some(
                self.waiting
                    .clone()
                    .unwrap_or_else(|| tr("No actions here").into()),
            )
        } else if visible.is_empty() {
            Some(tr("No matches").into())
        } else {
            None
        };
        popup::panel(ui)
            .key_context("ContextActions")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .on_key_down(cx.listener(Self::key_down))
            .min_w(px(MIN_WIDTH))
            .max_w(px(MAX_WIDTH))
            .p(px(PADDING))
            .flex()
            .flex_col()
            .when(!self.query.is_empty(), |panel| {
                panel.child(
                    div()
                        .h(px(ROW_HEIGHT - 4.))
                        .px_2()
                        .mb_1()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .rounded(px(RADIUS_SM))
                        .bg(ui.input_background)
                        .text_size(px(theme::TEXT_SM))
                        .child(icon(IconName::Search, ui.dim).size(px(12.)))
                        .child(self.query.clone()),
                )
            })
            // Loading, or why there is nothing from the servers: above Claude's rows.
            .when_some(status, |panel, status| {
                panel.child(
                    div()
                        .h(px(ROW_HEIGHT))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_color(ui.dim)
                        .child(icon(IconName::Bulb, ui.text_disabled).size(px(14.)))
                        .child(status),
                )
            })
            .child(
                div()
                    .id("context-actions-rows")
                    .max_h(px(ROW_HEIGHT * MAX_VISIBLE_ROWS as f32))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(items),
            )
    }
}

// --- Applying ---

/// Runs a chosen code action or command of `server`: resolve, edit, command.
fn run_action(
    _workspace: &mut Workspace,
    server: LanguageServer,
    item: CodeActionOrCommand,
    cx: &mut Context<Workspace>,
) {
    cx.spawn(async move |this, cx| {
        let action = match item {
            CodeActionOrCommand::Command(command) => {
                execute(&server, command, &this, cx).await;
                return;
            }
            CodeActionOrCommand::CodeAction(action) => action,
        };
        let action = resolve(&server, action).await;
        if let Some(edit) = action.edit.as_ref() {
            let applied = this.update(cx, |workspace, cx| {
                apply_edit(workspace, edit, server.clone(), cx)
            });
            let Ok(applied) = applied else {
                return;
            };
            if let Err(error) = applied.await {
                report(&this, trf("Couldn't apply “{0}”: {1}", &[&action.title, &error]), cx);
                return;
            }
        }
        if let Some(command) = action.command {
            execute(&server, command, &this, cx).await;
        }
    })
    .detach();
}

/// The action with its edit, when the server leaves it for `codeAction/resolve`.
async fn resolve(server: &LanguageServer, action: CodeAction) -> CodeAction {
    let resolvable = action.edit.is_none()
        && action.data.is_some()
        && server
            .capabilities()
            .is_some_and(|capabilities| resolves_code_actions(&capabilities));
    if !resolvable {
        return action;
    }
    match server
        .request::<CodeActionResolveRequest>(action.clone())
        .await
    {
        Ok(resolved) => resolved,
        Err(error) => {
            eprintln!("flux: codeAction/resolve of {}: {error}", server.name());
            action
        }
    }
}

/// `workspace/executeCommand`, when the server knows the command (client-side commands of a
/// server's own editor plugin — "trigger parameter hints" — are skipped).
async fn execute(
    server: &LanguageServer,
    command: Command,
    workspace: &WeakEntity<Workspace>,
    cx: &mut gpui::AsyncApp,
) {
    let known = server.capabilities().is_some_and(|capabilities| {
        capabilities
            .execute_command_provider
            .as_ref()
            .is_some_and(|provider| provider.commands.contains(&command.command))
    });
    if !known {
        return;
    }
    let title = command.title.clone();
    let result = server
        .request::<ExecuteCommand>(ExecuteCommandParams {
            command: command.command,
            arguments: command.arguments.unwrap_or_default(),
            work_done_progress_params: Default::default(),
        })
        .await;
    if let Err(error) = result
        && !error.is_outdated()
    {
        report(workspace, trf("“{0}” failed: {1}", &[&title, &error]), cx);
    }
}

fn report(workspace: &WeakEntity<Workspace>, message: String, cx: &mut gpui::AsyncApp) {
    workspace
        .update(cx, |workspace, cx| workspace.show_message(message.into(), cx))
        .ok();
}

/// A server's `workspace/applyEdit` (while it runs a command): applied like a code action's edit,
/// then answered.
pub(crate) fn apply_server_edit(
    workspace: &mut Workspace,
    server: LanguageServer,
    request: ApplyEdit,
    cx: &mut Context<Workspace>,
) {
    let applied = apply_edit(workspace, &request.params.edit, server, cx);
    cx.spawn(async move |_, _| match applied.await {
        Ok(()) => request.respond(true, None),
        Err(error) => request.respond(false, Some(error)),
    })
    .detach();
}

/// Applies a workspace edit: documents open in editors through their editors (one undo step
/// each), everything else — files on disk, created, renamed, deleted — in the background, in the
/// server's order; the server learns which files changed on disk. `Err` — why not everything was
/// applied (the open documents may already have their edits).
pub(crate) fn apply_edit(
    workspace: &mut Workspace,
    edit: &WorkspaceEdit,
    server: LanguageServer,
    cx: &mut Context<Workspace>,
) -> Task<Result<(), String>> {
    let changes = match flux_lsp::edit::workspace_changes(edit) {
        Ok(changes) => changes,
        Err(error) => return Task::ready(Err(error)),
    };
    // Files a file operation touches are left to the disk, after it.
    let operated: Vec<PathBuf> = changes
        .iter()
        .flat_map(|change| match change {
            FileChange::Edit { .. } => vec![],
            FileChange::Create { path, .. } | FileChange::Delete { path, .. } => {
                vec![canonical(path)]
            }
            FileChange::Rename { from, to, .. } => vec![canonical(from), canonical(to)],
        })
        .collect();
    let editors: Vec<(PathBuf, Entity<Editor>)> = workspace
        .editors(cx)
        .into_iter()
        .filter_map(|editor| {
            let path = canonical(editor.read(cx).document.path()?);
            Some((path, editor))
        })
        .collect();
    let open = |path: &Path| {
        let key = canonical(path);
        if operated.contains(&key) {
            return None;
        }
        editors
            .iter()
            .find(|(open, _)| *open == key)
            .map(|(_, editor)| editor.clone())
    };
    // Edits of open documents, by file in order: one undo step each.
    let mut in_editors: Vec<(Entity<Editor>, Vec<Vec<lsp_types::TextEdit>>)> = Vec::new();
    let mut on_disk: Vec<FileChange> = Vec::new();
    for change in changes {
        match change {
            FileChange::Edit { path, edits } => match open(&path) {
                Some(editor) => match in_editors.iter_mut().find(|(known, _)| *known == editor) {
                    Some((_, batches)) => batches.push(edits),
                    None => in_editors.push((editor, vec![edits])),
                },
                None => on_disk.push(FileChange::Edit { path, edits }),
            },
            operation => on_disk.push(operation),
        }
    }
    for (editor, batches) in in_editors {
        editor.update(cx, |editor, cx| {
            let document = &editor.document;
            let changes =
                crate::rename::document_edits(document.text(), &batches, document.line_ending());
            if !changes.is_empty() {
                editor.replace_ranges(changes, cx);
            }
        });
    }
    if on_disk.is_empty() {
        return Task::ready(Ok(()));
    }
    let moves: Vec<(PathBuf, PathBuf)> = on_disk
        .iter()
        .filter_map(|change| match change {
            FileChange::Rename { from, to, .. } => Some((from.clone(), to.clone())),
            _ => None,
        })
        .collect();
    let written = cx.background_spawn(async move { apply_on_disk(on_disk) });
    cx.spawn(async move |this, cx| {
        let (events, result) = written.await;
        if !events.is_empty() {
            server.notify::<DidChangeWatchedFiles>(DidChangeWatchedFilesParams { changes: events });
        }
        this.update(cx, |workspace, cx| {
            // Open documents moved with their files.
            for (from, to) in &moves {
                workspace.documents_moved(from, to, cx);
            }
            if let Err(error) = &result {
                let notification = Notification::warning(tr("Couldn't apply the edit"))
                    .body(error.clone())
                    .group(NotificationGroup::LanguageServers);
                workspace.notify(notification, cx);
            }
        })
        .ok();
        result
    })
}

/// The disk part of a workspace edit, in order; stops at the first failure (`failureHandling:
/// abort`). The file events for the server, and the result.
fn apply_on_disk(changes: Vec<FileChange>) -> (Vec<FileEvent>, Result<(), String>) {
    let mut events = Vec::new();
    let event = |path: &Path, typ| FileEvent {
        uri: position::uri_from_path(path),
        typ,
    };
    for change in changes {
        let result = match &change {
            FileChange::Edit { path, edits } => {
                crate::rename::apply_to_file(path, std::slice::from_ref(edits))
                    .map(|()| events.push(event(path, FileChangeType::CHANGED)))
                    .map_err(|error| format!("{}: {error}", path.display()))
            }
            FileChange::Create {
                path,
                overwrite,
                ignore_if_exists,
            } => {
                if path.exists() && !overwrite {
                    if *ignore_if_exists {
                        Ok(())
                    } else {
                        Err(format!("{} already exists", path.display()))
                    }
                } else {
                    path.parent()
                        .map_or(Ok(()), std::fs::create_dir_all)
                        .and_then(|()| std::fs::write(path, ""))
                        .map(|()| events.push(event(path, FileChangeType::CREATED)))
                        .map_err(|error| format!("{}: {error}", path.display()))
                }
            }
            FileChange::Rename {
                from,
                to,
                overwrite,
                ignore_if_exists,
            } => {
                if to.exists() && !overwrite {
                    if *ignore_if_exists {
                        Ok(())
                    } else {
                        Err(format!("{} already exists", to.display()))
                    }
                } else {
                    to.parent()
                        .map_or(Ok(()), std::fs::create_dir_all)
                        .and_then(|()| std::fs::rename(from, to))
                        .map(|()| {
                            events.push(event(from, FileChangeType::DELETED));
                            events.push(event(to, FileChangeType::CREATED));
                        })
                        .map_err(|error| format!("{}: {error}", from.display()))
                }
            }
            FileChange::Delete {
                path,
                recursive,
                ignore_if_not_exists,
            } => {
                if !path.exists() {
                    if *ignore_if_not_exists {
                        Ok(())
                    } else {
                        Err(format!("{} doesn't exist", path.display()))
                    }
                } else {
                    // To the Trash, as the tree deletes: a refactoring can be undone by hand.
                    trash(path, *recursive)
                        .map(|()| events.push(event(path, FileChangeType::DELETED)))
                        .map_err(|error| format!("{}: {error}", path.display()))
                }
            }
        };
        if let Err(error) = result {
            return (events, Err(error));
        }
    }
    (events, Ok(()))
}

/// Moves a file (or, `recursive`, a folder) to the Trash.
fn trash(path: &Path, recursive: bool) -> std::io::Result<()> {
    if path.is_dir() && !recursive && std::fs::read_dir(path)?.next().is_some() {
        return Err(std::io::Error::other("the folder isn't empty"));
    }
    flux_fs::ops::trash(&[path.to_path_buf()])
}

/// Sorting for tests: rows' titles in display order.
#[cfg(test)]
fn titles(rows: &[Row]) -> Vec<&str> {
    rows.iter().map(|row| row.title.as_ref()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_follow_the_kinds() {
        let kind = |k: &str| Some(CodeActionKind::from(k.to_string()));
        assert_eq!(Group::of(kind("quickfix").as_ref(), true), Group::PreferredFix);
        assert_eq!(Group::of(kind("quickfix").as_ref(), false), Group::QuickFix);
        assert_eq!(Group::of(kind("refactor.extract").as_ref(), false), Group::Refactor);
        assert_eq!(Group::of(kind("source.organizeImports").as_ref(), false), Group::Source);
        assert_eq!(Group::of(kind("sourcery").as_ref(), false), Group::Other);
        assert_eq!(Group::of(None, true), Group::QuickFix);
        assert_eq!(Group::of(None, false), Group::Other);
    }

    #[test]
    fn fixes_come_first_disabled_last_in_their_group() {
        // `rows_of` needs a server handle; the sort works on the groups alone.
        let make = |title: &str, group, disabled: bool| Row {
            title: SharedString::from(title.to_string()),
            group,
            disabled: disabled.then(|| SharedString::from("no")),
            action: RowAction::Dispatch(Box::new(ShowContextActions)),
        };
        let mut rows = vec![
            make("Extract into function", Group::Refactor, true),
            make("Inline variable", Group::Refactor, false),
            make("Organize imports", Group::Source, false),
            make("Fix with Claude", Group::Claude, false),
            make("Import HashMap", Group::QuickFix, false),
            make("Add missing match arms", Group::PreferredFix, false),
        ];
        sort_rows(&mut rows);
        assert_eq!(
            titles(&rows),
            [
                "Add missing match arms",
                "Import HashMap",
                "Inline variable",
                "Extract into function",
                "Organize imports",
                "Fix with Claude",
            ]
        );
    }

    #[test]
    fn speed_search_takes_the_letters_in_order() {
        assert!(matches("Add missing match arms", "amm"));
        assert!(matches("Add missing match arms", "MATCH"));
        assert!(matches("Import `HashMap`", "hash map"));
        assert!(!matches("Inline variable", "xyz"));
        assert!(matches("Anything", ""));
        assert!(matches("Исправить с Claude", "испр"));
    }

    #[test]
    fn diagnostics_over_the_caret_or_the_selection() {
        let make = |range: std::ops::Range<usize>| Diagnostic {
            range,
            severity: Severity::Error,
            message: "m".into(),
            source: None,
            code: None,
            owner: 1,
        };
        let items = [make(4..8), make(10..10), make(20..25)];
        let at = |range: std::ops::Range<usize>| {
            overlapping(items.iter(), range)
                .map(|d| d.range.start)
                .collect::<Vec<_>>()
        };
        // The caret inside, at either end of a problem, or at an empty one.
        assert_eq!(at(5..5), [4]);
        assert_eq!(at(8..8), [4]);
        assert_eq!(at(10..10), [10]);
        assert_eq!(at(15..15), Vec::<usize>::new());
        // A selection across problems.
        assert_eq!(at(6..21), [4, 10, 20]);
        assert_eq!(at(8..10), Vec::<usize>::new());
    }

    #[test]
    fn a_published_problem_is_found_by_message_code_and_source() {
        let ours = Diagnostic {
            range: 0..1,
            severity: Severity::Warning,
            message: "`os` imported but unused".into(),
            source: Some("Ruff".into()),
            code: Some("F401".into()),
            owner: 2,
        };
        let raw = lsp_types::Diagnostic {
            message: "`os` imported but unused".into(),
            source: Some("Ruff".into()),
            code: Some(NumberOrString::String("F401".into())),
            data: Some(serde_json::json!({ "fix": "remove" })),
            ..Default::default()
        };
        assert!(same_problem(&raw, &ours));
        let other = lsp_types::Diagnostic {
            message: "something else".into(),
            ..raw.clone()
        };
        assert!(!same_problem(&other, &ours));
    }

    #[test]
    fn disk_steps_run_in_order_and_stop_at_a_failure() {
        let dir = std::env::temp_dir().join(format!("flux-code-actions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let new = dir.join("sub/new.rs");
        let edit = |text: &str| lsp_types::TextEdit {
            range: lsp_types::Range::default(),
            new_text: text.into(),
        };
        let (events, result) = apply_on_disk(vec![
            FileChange::Create {
                path: new.clone(),
                overwrite: false,
                ignore_if_exists: false,
            },
            FileChange::Edit {
                path: new.clone(),
                edits: vec![edit("fn moved() {}\n")],
            },
            FileChange::Rename {
                from: new.clone(),
                to: dir.join("moved.rs"),
                overwrite: false,
                ignore_if_exists: false,
            },
        ]);
        assert_eq!(result, Ok(()));
        assert_eq!(events.len(), 4);
        assert_eq!(
            std::fs::read_to_string(dir.join("moved.rs")).unwrap(),
            "fn moved() {}\n"
        );
        // An existing file isn't created over: the steps after it don't run.
        let (_, result) = apply_on_disk(vec![
            FileChange::Create {
                path: dir.join("moved.rs"),
                overwrite: false,
                ignore_if_exists: false,
            },
            FileChange::Edit {
                path: dir.join("moved.rs"),
                edits: vec![edit("// no\n")],
            },
        ]);
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(dir.join("moved.rs")).unwrap(),
            "fn moved() {}\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
