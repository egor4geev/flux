//! The sections of the plugin API 0.3: forms, pickers, the settings page's kinds, branches and the
//! commit template, links on keys like `PLAY-12`, Claude sessions, contexts of tabs, the local
//! time, badges and a multi-line field, a window among the editor's tabs. What they return lands in
//! their sections, as the 0.2 ones' does.

use flux_plugin_api::dialog::{Form, Values};
use flux_plugin_api::host::ui;
use flux_plugin_api::picker::{self, Picker, item};
use flux_plugin_api::view::*;
use flux_plugin_api::{
    CommandContext, Event, UiEvent, claude, contexts, git, links, secrets, setting, settings,
    system, tr, trf,
};
use serde::Deserialize;

use crate::{CLOCK, Playground, Section, WINDOW, clip, failed};

/// The window among the editor's tabs (`location = "editor"` in the manifest).
pub(crate) const BOARD: &str = "board";
/// The settings of the manifest the Settings Page section reads and writes.
const TOKEN: &str = "playground-token";
const STATUS: &str = "playground-status";
const NOTE: &str = "playground-note";
const BOOKMARKS: &str = "bookmarks";

/// Tasks for the pickers and the board: key, title, status.
const TASKS: &[(&str, &str, &str)] = &[
    ("PLAY-1", "Sign in with Yandex ID", "Done"),
    ("PLAY-2", "Show my tasks by status", "Done"),
    ("PLAY-3", "Open a task's card", "Review"),
    ("PLAY-4", "Comments with a field for a new one", "In Progress"),
    ("PLAY-5", "History of a task's changes", "In Progress"),
    ("PLAY-6", "Links between tasks", "Open"),
    ("PLAY-7", "A board of tasks in an editor tab", "Open"),
    ("PLAY-8", "Take a task to work in one click", "Open"),
    ("PLAY-9", "A prompt for Claude from the settings", "Open"),
    ("PLAY-10", "A branch per task", "Review"),
    ("PLAY-11", "Log the time spent", "Open"),
    ("PLAY-12", "Remind about a meeting", "In Progress"),
];

/// The statuses of the board's columns, in order.
const STATUSES: &[&str] = &["Open", "In Progress", "Review", "Done"];

/// What the 0.3 sections remember between calls.
#[derive(Default)]
pub(crate) struct State {
    form: Option<u64>,
    live_picker: Option<u64>,
    filtered_picker: Option<u64>,
    clock_menu: Option<u64>,
    badge_menu: Option<u64>,
    /// The badge's status in the Pickers section: an index of [`STATUSES`].
    badge: usize,
    /// A branch made by New Branch: (the repository's root, the branch it came from).
    branched_from: Option<(String, String)>,
    /// A `new-branch` or `checkout` under way: its id and what it does.
    git_operation: Option<(u64, String)>,
    branches_made: u32,
    template: bool,
    /// The context of tabs on screen, A or B; none — neither yet.
    context: Option<&'static str>,
    board_shown: bool,
    renames: u32,
    comment_changes: u32,
}

/// A record of the `bookmarks` setting.
#[derive(Debug, Deserialize)]
struct Bookmark {
    #[serde(default)]
    name: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    pinned: bool,
}

impl Playground {
    /// The 0.3 events: answered here (none), or handed back for the 0.2 sections.
    pub(crate) fn handle_03(&mut self, event: Event) -> Option<Event> {
        match event {
            Event::FormAnswered((id, answer)) if Some(id) == self.v3.form => {
                self.v3.form = None;
                self.form_answered(answer.map(Values::from));
            }
            Event::PickerQuery((id, query)) if Some(id) == self.v3.live_picker => {
                picker::update(id, task_items(&query), false);
            }
            Event::PickerChosen((id, key)) => self.picker_chosen(id, key),
            Event::LinkHovered((request, _, text)) => {
                let title = TASKS
                    .iter()
                    .find(|(key, _, _)| *key == text)
                    .map_or_else(|| tr("Not a playground task"), |(_, title, _)| title.to_string());
                links::tooltip(
                    request,
                    &format!("**{text}** · {title}\n\n{}", tr("The tooltip comes from the plugin.")),
                );
            }
            Event::LinkActivated((pattern, text)) => {
                self.set(
                    Section::Links,
                    trf("Activated {0} (the pattern \"{1}\").", &[&text, &pattern]),
                );
                ui::show(WINDOW);
            }
            Event::GitDone((id, result))
                if Some(id) == self.v3.git_operation.as_ref().map(|(id, _)| *id) =>
            {
                let (_, what) = self.v3.git_operation.take().unwrap_or_default();
                let text = match result {
                    Ok(()) => trf("{0}: done.", &[&what]),
                    Err(err) => format!("{what}: {}", failed(&err)),
                };
                self.set(Section::Branches, text);
            }
            Event::ToolWindowShown(window) if window == BOARD => {
                self.v3.board_shown = true;
                self.render_board();
                self.set(Section::EditorWindows, tr("The board is open among the editor's tabs."));
            }
            Event::ToolWindowHidden(window) if window == BOARD => {
                self.v3.board_shown = false;
                self.set(Section::EditorWindows, tr("The board's tab is closed."));
            }
            Event::Ui(input) if input.window == BOARD => match input.event {
                UiEvent::Selected(key) => {
                    self.set(Section::EditorWindows, trf("Selected {0} on the board.", &[&key]))
                }
                UiEvent::Activated(key) => {
                    self.set(Section::EditorWindows, trf("Opened {0} on the board.", &[&key]))
                }
                UiEvent::Clicked if input.element == "board-rename" => self.rename_board(),
                _ => {}
            },
            Event::Ui(input) if input.window == WINDOW && input.element == "widgets-comment" => {
                match input.event {
                    UiEvent::Changed(_) => self.v3.comment_changes += 1,
                    UiEvent::Submitted(text) => {
                        self.set(
                            Section::Widgets,
                            trf(
                                "Submitted with ⌘↵ after {0} changes: {1} lines\n\n> {2}",
                                &[
                                    &self.v3.comment_changes,
                                    &text.lines().count(),
                                    &clip(&text.replace('\n', " ⏎ "), 200),
                                ],
                            ),
                        );
                        self.v3.comment_changes = 0;
                    }
                    _ => {}
                }
            }
            event => return Some(event),
        }
        None
    }

    /// The 0.3 sections' buttons.
    pub(crate) fn clicked_03(&mut self, element: &str) {
        match element {
            "forms-open-task" => self.ask_open_task(None, None),
            "pickers-live" => self.open_live_picker(),
            "pickers-filtered" => self.open_filtered_picker(),
            "pickers-clock" => self.open_clock_menu(),
            "pickers-status" => {
                self.v3.badge_menu = Some(
                    Picker::menu()
                        .items(STATUSES.iter().enumerate().map(|(index, status)| {
                            let status = item(&index.to_string(), &tr(status));
                            match index == self.v3.badge {
                                true => status.badge("✓"),
                                false => status,
                            }
                        }))
                        .under(WINDOW, "pickers-status")
                        .open(),
                );
            }
            "settings-status" => {
                let now = system::local_time(system::now()).hm();
                settings::status(
                    STATUS,
                    &trf("Written by the playground at {0}", &[&now]),
                    settings::Tone::Success,
                );
                self.set(
                    Section::SettingsPage,
                    tr("The status line is written: see Settings → the playground."),
                );
            }
            "settings-busy" => {
                settings::busy(STATUS, &tr("Checking…"));
                self.set(Section::SettingsPage, tr("The status line has a spinner now."));
            }
            "settings-set" => {
                let now = system::local_time(system::now()).hm();
                let note = trf("Set by the plugin at {0}.\nA second line.", &[&now]);
                let text = match settings::set(NOTE, &note) {
                    Ok(()) => tr("The note is set: the open page shows it at once."),
                    Err(err) => failed(&err),
                };
                self.set(Section::SettingsPage, text);
            }
            "settings-read" => self.read_settings(),
            "settings-open" => system::open_settings(),
            "branches-list" => self.list_branches(),
            "branches-new" => self.new_branch(),
            "branches-back" => self.branch_back(),
            "branches-template" => self.toggle_template(),
            "links-set" => {
                let pattern = links::pattern("play", &links::key_regex(&["PLAY"])).tooltip();
                let text = match links::set([pattern]) {
                    Ok(()) => tr(
                        "PLAY-12 and the like are links now: in a file (⌘ + hover, ⌘-click), a \
                         commit message, the Git log, Claude's chat. A hover shows the plugin's \
                         tooltip; a click comes here.",
                    ),
                    Err(err) => failed(&err),
                };
                self.set(Section::Links, text);
            }
            "links-bad" => {
                let text = match links::set([links::pattern("broken", "PLAY-(\\d+")]) {
                    Ok(()) => tr("Unexpected: the broken pattern was taken."),
                    Err(err) => format!("{} ✓\n\n> {err}", tr("Rejected, as it should be.")),
                };
                self.set(Section::Links, text);
            }
            "links-clear" => {
                links::clear();
                self.set(Section::Links, tr("No patterns: PLAY-12 is plain text again."));
            }
            "claude-discuss" => self.claude(false),
            "claude-plan" => self.claude(true),
            "contexts-switch" => self.switch_context(),
            "contexts-forget" => {
                contexts::remove("a");
                contexts::remove("b");
                self.v3.context = None;
                self.set(Section::Contexts, tr("Both contexts are forgotten."));
            }
            "time-now" => self.local_time(),
            "board-open" => ui::show(BOARD),
            "board-rename" => self.rename_board(),
            "board-close" => ui::hide(BOARD),
            _ => {}
        }
    }

    /// The settings page's button (`type = "button"`): its command with the button's argument.
    pub(crate) fn settings_check(&mut self, context: &CommandContext) {
        let argument = context.argument().unwrap_or("—").to_string();
        settings::status(
            STATUS,
            &trf("Checked from {0}: \"{1}\"", &[&context.source.name(), &argument]),
            settings::Tone::Success,
        );
        self.set(
            Section::SettingsPage,
            trf(
                "**settings-check** ran from `{0}` with the argument «{1}».",
                &[&context.source.name(), &argument],
            ),
        );
    }

    /// The 0.3 sections: their buttons and what else they show under them.
    pub(crate) fn sections_03(&self) -> Vec<(Section, Vec<Element>, Vec<Element>)> {
        let b = |id: &str, label: &str| -> Element { button(id, &tr(label)).into() };
        let template = if self.v3.template { "Template Off" } else { "Template On" };
        let status = STATUSES[self.v3.badge];
        vec![
            (Section::Forms, vec![b("forms-open-task", "Open Task…")], Vec::new()),
            (
                Section::Pickers,
                vec![
                    b("pickers-live", "Search"),
                    b("pickers-filtered", "Filter"),
                    b("pickers-clock", "At the Clock"),
                ],
                vec![row(
                    "pickers-badge-row",
                    [
                        label("pickers-badge-label", &tr("A status with transitions:")),
                        menu_badge("pickers-status", &tr(status), status_tone(status)),
                    ],
                )],
            ),
            (
                Section::SettingsPage,
                vec![
                    b("settings-status", "Status"),
                    b("settings-busy", "Busy"),
                    b("settings-set", "Set Note"),
                    b("settings-read", "Read"),
                    b("settings-open", "Open Settings"),
                ],
                Vec::new(),
            ),
            (
                Section::Branches,
                vec![
                    b("branches-list", "List"),
                    b("branches-new", "New Branch"),
                    b("branches-back", "Back"),
                    b("branches-template", template),
                ],
                Vec::new(),
            ),
            (
                Section::Links,
                vec![
                    b("links-set", "Link PLAY-n"),
                    b("links-bad", "Bad Pattern"),
                    b("links-clear", "Clear"),
                ],
                Vec::new(),
            ),
            (
                Section::Claude,
                vec![b("claude-discuss", "Discuss"), b("claude-plan", "Plan")],
                Vec::new(),
            ),
            (
                Section::Contexts,
                vec![b("contexts-switch", "Switch A ↔ B"), b("contexts-forget", "Forget")],
                Vec::new(),
            ),
            (Section::Time, vec![b("time-now", "Local Time")], Vec::new()),
            (
                Section::Widgets,
                Vec::new(),
                vec![
                    row(
                        "widgets-badges",
                        STATUSES
                            .iter()
                            .enumerate()
                            .map(|(index, status)| {
                                let id = format!("widgets-badge-{index}");
                                badge(&id, &tr(status), status_tone(status))
                            })
                            .chain([badge("widgets-badge-error", &tr("Blocked"), Tone::Error)]),
                    ),
                    multiline_field(
                        "widgets-comment",
                        "",
                        Some(&tr("A comment: ↵ — a new line, ⌘↵ — send")),
                    ),
                ],
            ),
            (
                Section::EditorWindows,
                vec![
                    b("board-open", "Open Board"),
                    b("board-rename", "Rename"),
                    b("board-close", "Close"),
                ],
                Vec::new(),
            ),
        ]
    }

    // --- Forms ---

    /// A form as JetBrains' Open Task: the status, a branch, the tabs, a comment. `values` — the
    /// user's answer to ask again with, `error` — what was wrong with it.
    fn ask_open_task(&mut self, error: Option<&str>, values: Option<&Values>) {
        let value = |key: &str, default: &str| -> String {
            values
                .and_then(|values| values.get(key))
                .unwrap_or(default)
                .to_string()
        };
        let on = |key: &str| values.is_none_or(|values| values.on(key));
        let statuses = [
            ("in-progress", tr("In Progress")),
            ("review", tr("Review")),
            ("keep", tr("Don't change")),
        ];
        let options: Vec<(&str, &str)> =
            statuses.iter().map(|(value, title)| (*value, title.as_str())).collect();
        let mut form = Form::new(&tr("Open Task"), &tr("Start"))
            .message(&tr("PLAY-12 · Remind about a meeting"))
            .choice("status", &tr("Move to"), &options, Some(value("status", "in-progress").as_str()))
            .toggle("branch", &tr("Create a branch"), on("branch"))
            .text(
                "branch-name",
                &tr("Branch"),
                &value("branch-name", "PLAY-12-remind-about-a-meeting"),
            )
            .code()
            .enabled_by("branch")
            .placeholder("PLAY-12-…")
            .toggle("tabs", &tr("Switch to the task's tabs"), on("tabs"))
            .multiline("comment", &tr("Comment"), &value("comment", ""))
            .placeholder(&tr("Goes to the task's history; optional"))
            .note(
                "hint",
                &tr("Empty the branch's name with Create a branch on: the form asks again."),
            );
        if let Some(error) = error {
            form = form.error(error);
        }
        self.v3.form = Some(form.send());
    }

    fn form_answered(&mut self, answer: Option<Values>) {
        let Some(values) = answer else {
            self.set(Section::Forms, tr("Cancelled."));
            return;
        };
        if values.on("branch") && values.text("branch-name").is_empty() {
            let error = tr("Name the branch, or turn Create a branch off.");
            self.ask_open_task(Some(&error), Some(&values));
            return;
        }
        let comment = values.text("comment");
        let text = format!(
            "- {}: `{}`\n- {}: {}\n- {}: {}\n- {}: {}",
            tr("Move to"),
            values.text("status"),
            tr("Branch"),
            if values.on("branch") {
                format!("`{}`", values.text("branch-name"))
            } else {
                tr("no")
            },
            tr("Switch the tabs"),
            if values.on("tabs") { tr("yes") } else { tr("no") },
            tr("Comment"),
            if comment.is_empty() {
                "—".to_string()
            } else {
                format!(
                    "«{}» ({} {})",
                    clip(&comment.replace('\n', " ⏎ "), 120),
                    comment.lines().count(),
                    tr("lines")
                )
            },
        );
        self.set(Section::Forms, text);
    }

    // --- Pickers ---

    fn open_live_picker(&mut self) {
        self.v3.live_picker = Some(
            Picker::new(&tr("Find a task by its key or words"))
                .live()
                .items(task_items(""))
                .empty_text(&tr("No such task"))
                .open(),
        );
        self.set(
            Section::Pickers,
            tr("A live picker: the plugin answers every query (picker-query → update)."),
        );
    }

    fn open_filtered_picker(&mut self) {
        self.v3.filtered_picker = Some(
            Picker::new(&tr("Filter the tasks"))
                .items(task_items(""))
                .empty_text(&tr("No such task"))
                .open(),
        );
        self.set(Section::Pickers, tr("Flux filters this one by the query, fuzzily."));
    }

    fn open_clock_menu(&mut self) {
        self.v3.clock_menu = Some(
            Picker::menu()
                .item(item("stop", &tr("Stop the Clock")).icon("clock"))
                .item(item("utc", &tr("What time is it in UTC?")))
                .at_status_item(CLOCK)
                .open(),
        );
        self.set(
            Section::Pickers,
            tr("A menu above the clock in the status bar (under the title bar when it isn't shown)."),
        );
    }

    fn picker_chosen(&mut self, id: u64, key: Option<String>) {
        let chosen = key.as_deref().unwrap_or_default();
        if Some(id) == self.v3.badge_menu {
            self.v3.badge_menu = None;
            if let Some(index) = key.and_then(|key| key.parse::<usize>().ok()) {
                self.v3.badge = index.min(STATUSES.len() - 1);
            }
            self.render();
            return;
        }
        let which = if Some(id) == self.v3.live_picker {
            self.v3.live_picker = None;
            tr("Search")
        } else if Some(id) == self.v3.filtered_picker {
            self.v3.filtered_picker = None;
            tr("Filter")
        } else if Some(id) == self.v3.clock_menu {
            self.v3.clock_menu = None;
            if chosen == "stop" && self.clock.is_some() {
                self.toggle_clock();
            }
            tr("At the Clock")
        } else {
            return;
        };
        let text = match key {
            Some(key) => trf("{0}: chose `{1}`.", &[&which, &key]),
            None => trf("{0}: dismissed.", &[&which]),
        };
        self.set(Section::Pickers, text);
    }

    // --- The settings page ---

    fn read_settings(&mut self) {
        let token = match secrets::get(TOKEN) {
            Some(token) => trf("saved ({0} characters)", &[&token.chars().count()]),
            None => tr("not saved"),
        };
        let note: String = setting(NOTE).unwrap_or_default();
        let bookmarks: Vec<Bookmark> = setting(BOOKMARKS).unwrap_or_default();
        let mut text = format!(
            "- {}: {token}\n- {}: «{}»\n- {}: {}",
            tr("The token (a secret)"),
            tr("The note (text)"),
            clip(&note.replace('\n', " ⏎ "), 120),
            tr("Bookmarks (records)"),
            bookmarks.len()
        );
        for bookmark in &bookmarks {
            text.push_str(&format!(
                "\n  - **{}** `{}` · {}{}",
                bookmark.name,
                bookmark.url,
                bookmark.kind,
                if bookmark.pinned { " · 📌" } else { "" }
            ));
        }
        self.set(Section::SettingsPage, text);
    }

    // --- Branches ---

    /// The first repository of the project: its root and current branch.
    fn repository() -> Option<(String, Option<String>)> {
        git::repositories()
            .into_iter()
            .next()
            .map(|repo| (repo.root, repo.branch))
    }

    fn list_branches(&mut self) {
        let Some((root, _)) = Self::repository() else {
            self.set(Section::Branches, tr("No repositories (or no `project` permission)."));
            return;
        };
        let text = match git::branches(&root) {
            Ok(branches) => {
                let mut lines: Vec<String> = branches
                    .iter()
                    .take(15)
                    .map(|branch| {
                        format!(
                            "- `{}`{}{}",
                            branch.name,
                            if branch.remote { " · remote" } else { "" },
                            if branch.current { " · **current**" } else { "" }
                        )
                    })
                    .collect();
                if branches.len() > 15 {
                    lines.push(trf("- … {0} more", &[&(branches.len() - 15)]));
                }
                format!("{}: {}\n\n{}", tr("Branches"), branches.len(), lines.join("\n"))
            }
            Err(err) => failed(&err),
        };
        self.set(Section::Branches, text);
    }

    fn new_branch(&mut self) {
        let Some((root, Some(branch))) = Self::repository() else {
            self.set(Section::Branches, tr("A repository on a branch is needed."));
            return;
        };
        self.v3.branches_made += 1;
        let name = format!("playground/{}", self.v3.branches_made);
        let id = git::new_branch(&root, &name, None);
        self.v3.branched_from = Some((root, branch.clone()));
        self.v3.git_operation = Some((id, trf("New branch {0} from {1}", &[&name, &branch])));
        self.set(Section::Branches, trf("Creating {0}… (git-done says how it ended)", &[&name]));
    }

    fn branch_back(&mut self) {
        let Some((root, branch)) = self.v3.branched_from.clone() else {
            self.set(Section::Branches, tr("Make a branch with New Branch first."));
            return;
        };
        let id = git::checkout(&root, &branch);
        self.v3.git_operation = Some((id, trf("Checkout {0}", &[&branch])));
        self.set(Section::Branches, trf("Checking out {0}…", &[&branch]));
    }

    fn toggle_template(&mut self) {
        self.v3.template = !self.v3.template;
        let text = if self.v3.template {
            git::commit_template("PLAY-12 Remind about a meeting", Some("PLAY-12"));
            tr(
                "The commit window's empty message gets «PLAY-12 Remind about a meeting»; a \
                 message generated with Claude starts with PLAY-12.",
            )
        } else {
            git::clear_commit_template();
            tr("No template: the commit window's message is the user's again.")
        };
        self.set(Section::Branches, text);
    }

    // --- Claude ---

    fn claude(&mut self, plan: bool) {
        let task = "**PLAY-12 · Remind about a meeting**\n\nA notification a few minutes before a \
                    meeting of the user's calendar, with Join when it has a Telemost link.\n\n\
                    Comments:\n- Anna: the minutes before it — a setting.";
        let mut session = claude::session()
            .title("PLAY-12: Remind about a meeting")
            .attach("PLAY-12", task);
        session = if plan {
            session
                .prompt(&tr("Study the task and the code it concerns, then propose a plan."))
                .send()
                .plan_mode()
        } else {
            session.prompt(&tr("What would you change first?"))
        };
        let text = match session.start() {
            Ok(()) if plan => tr("A session in plan mode: the task and the request went at once."),
            Ok(()) => tr("A session with the task attached; the request waits in the field."),
            Err(err) => failed(&err),
        };
        self.set(Section::Claude, text);
    }

    // --- Contexts ---

    fn switch_context(&mut self) {
        let (save, load) = match self.v3.context {
            Some("a") => (("a", tr("Playground A")), "b"),
            _ => (("b", tr("Playground B")), "a"),
        };
        let text = match contexts::switch(Some((save.0, save.1.as_str())), Some(load)) {
            Ok(()) => {
                self.v3.context = Some(load);
                trf(
                    "Saved the tabs as «{0}» and opened {1}'s tabs (none until it is saved). Load \
                     Context… lists both.",
                    &[&save.1, &load.to_uppercase()],
                )
            }
            Err(err) => failed(&err),
        };
        self.set(Section::Contexts, text);
    }

    // --- Time ---

    fn local_time(&mut self) {
        let now = system::now();
        let local = system::local_time(now);
        let offset = system::utc_offset(now);
        let text = format!(
            "- {}: `{}`\n- {}: UTC{}{:02}:{:02}\n- {}: `{}` · {}",
            tr("Time zone"),
            system::time_zone(),
            tr("Offset"),
            if offset < 0 { '−' } else { '+' },
            offset.unsigned_abs() / 3600,
            offset.unsigned_abs() / 60 % 60,
            tr("Local time"),
            local.iso(),
            local.hm(),
        );
        self.set(Section::Time, text);
    }

    // --- A window in the editor ---

    fn rename_board(&mut self) {
        self.v3.renames += 1;
        let title = trf("Playground Board {0}", &[&self.v3.renames]);
        ui::set_title(BOARD, &title);
        self.set(Section::EditorWindows, trf("Renamed the board's tab to «{0}».", &[&title]));
    }

    /// The board: a column of tasks per status, side by side.
    pub(crate) fn render_board(&self) {
        if !self.v3.board_shown {
            return;
        }
        let columns = STATUSES.iter().map(|status| {
            let mut tree = Tree::new().empty_text(&tr("No tasks"));
            for (key, title, _) in TASKS.iter().filter(|(_, _, task)| task == status) {
                let label = vec![span(key).code().tone(Tone::Accent), span(&format!(" {title}"))];
                tree.add(RowSpec::spans(key, label).icon("icons/playground.svg"));
            }
            column(
                &format!("board-column-{status}"),
                [
                    row(
                        &format!("board-heading-{status}"),
                        [badge(&format!("board-badge-{status}"), &tr(status), status_tone(status))],
                    ),
                    tree.into_element(&format!("board-tasks-{status}")),
                ],
            )
        });
        set_view(
            BOARD,
            column(
                "board-root",
                [
                    toolbar(
                        "board-toolbar",
                        [icon_button("board-rename", "pencil", &tr("Rename the Tab"))],
                    ),
                    row("board-columns", columns),
                ],
            ),
        );
    }
}

/// The tasks as picker items, grouped by status, those matching `query` (a key or words).
fn task_items(query: &str) -> Vec<picker::Item> {
    let query = query.trim().to_lowercase();
    TASKS
        .iter()
        .filter(|(key, title, _)| {
            query.is_empty()
                || key.to_lowercase().contains(&query)
                || title.to_lowercase().contains(&query)
        })
        .map(|(key, title, status)| {
            item(key, title)
                .spans([span(key).code(), span(&format!(" {title}"))])
                .group(&tr(status))
                .badge(&tr(status))
        })
        .collect()
}

/// A status's color.
fn status_tone(status: &str) -> Tone {
    match status {
        "In Progress" => Tone::Warning,
        "Review" => Tone::Accent,
        "Done" => Tone::Success,
        _ => Tone::Muted,
    }
}
