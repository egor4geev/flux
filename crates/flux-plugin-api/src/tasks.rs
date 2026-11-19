//! The user's tasks between plugins (API 0.6). A tracker's plugin gives them — `task-source =
//! true` in its manifest — and answers each `Event::TaskQuery` with [`respond`]; another plugin
//! (the `tasks` permission) asks Flux with [`mine`], [`search`] or [`get`], which wait for every
//! source (up to ten seconds) and merge what they gave, each task marked with its `source` (the
//! plugin that gave it). The query goes to the source's service when it has one, else to its
//! instance in a window; never to the instance that asks.
//!
//! A task of [`get`] has the source's Claude actions on it (`actions`: a tracker's «Take to
//! Work», the user's own ones — a name, plan mode, whether the task is taken to work first), and
//! the asker runs one with [`run`] in a project: Flux opens the project's window in the background
//! when none has it and sends `Query::Run` to the source's instance in that window — the work is
//! the window's: Git, the tabs, Claude's chat. The source does what the action does at the
//! computer, but asks nothing — nobody is there: no questions, forms or pickers, Git's
//! `…_quietly` calls — and answers with [`started`]: the Claude session it opened, and a note on
//! whatever it left out.
//!
//! ```ignore
//! // The tracker (the source):
//! fn on_event(&mut self, event: Event) {
//!     if let Event::TaskQuery(query) = event {
//!         let found = match query.query {
//!             Query::Mine => self.my_tasks(),
//!             Query::Search(text) => self.search(&text),
//!             // With the text, the comments and the actions.
//!             Query::Get(key) => self.task(&key).map(|task| {
//!                 vec![task.action(tasks::action("work", "Take to Work").start())]
//!             }),
//!             // In the project's window: the branch quietly, then Claude — on `GitDone`.
//!             Query::Run(run) => return self.start_run(query.id, run),
//!         };
//!         tasks::respond(query.id, found);
//!     }
//! }
//! // …the branch failed: Claude starts on the current one, and the bot says why.
//! let id = claude::session().title(&title).attach(&key, &text).prompt(&prompt).send().start();
//! tasks::started(query, id.map(|id| tasks::opened(&id).note(&tr("The branch wasn't checked out"))));
//!
//! // The bot (the asker):
//! for task in tasks::mine()? {
//!     // task.key, task.title, task.status, task.source
//! }
//! let task = tasks::get("TRACK-142")?.ok_or("no such task")?;
//! let started = tasks::run(&task, &task.actions[0].id, "/Users/me/app")?;
//! // started.session — the Claude session's id; started.notes — what went otherwise.
//! ```

pub use crate::host::tasks::{Query, Task, TaskAction, TaskQuery, TaskRun, TaskStarted};

use crate::host::tasks as raw;

/// The user's open tasks, from every source.
pub fn mine() -> Result<Vec<Task>, String> {
    raw::find(&Query::Mine)
}

/// The tasks the user's words find.
pub fn search(text: &str) -> Result<Vec<Task>, String> {
    raw::find(&Query::Search(text.to_string()))
}

/// One task by its key, with its text and comments (`markdown`) and the source's actions on it;
/// none when no source knows it.
pub fn get(key: &str) -> Result<Option<Task>, String> {
    Ok(raw::find(&Query::Get(key.to_string()))?.into_iter().next())
}

/// Runs one of the task's actions (its `id` among the task's `actions`) in a project — the folder
/// of one Flux knows; its window opens in the background when none has it — and waits for it, up
/// to a minute: the Claude session it opened, and what went otherwise than at the computer.
pub fn run(task: &Task, action: &str, project: &str) -> Result<TaskStarted, String> {
    run_with(&TaskRun {
        source: task.source.clone(),
        key: task.key.clone(),
        action: action.to_string(),
        project: project.to_string(),
    })
}

/// [`run`] by its parts: a task kept by its key and its source.
pub fn run_with(run: &TaskRun) -> Result<TaskStarted, String> {
    raw::run(run)
}

/// A source's answer to `Event::TaskQuery` (by its id).
pub fn respond(query: u64, tasks: Result<Vec<Task>, String>) {
    match &tasks {
        Ok(tasks) => raw::respond(query, Ok(tasks)),
        Err(error) => raw::respond(query, Err(error)),
    }
}

/// A source's answer to `Query::Run` (by the query's id): the session the action opened
/// ([`opened`]), or why it couldn't run.
pub fn started(query: u64, result: Result<TaskStarted, String>) {
    match &result {
        Ok(started) => raw::started(query, Ok(started)),
        Err(error) => raw::started(query, Err(error)),
    }
}

/// What a run did, for [`started`]: the Claude session it opened (`claude::Session::start`'s id);
/// notes with [`TaskStarted::note`].
pub fn opened(session: &str) -> TaskStarted {
    TaskStarted {
        session: session.to_string(),
        notes: Vec::new(),
    }
}

impl TaskStarted {
    /// What went otherwise than at the computer, a sentence in the user's language: "The branch
    /// TRACK-142-fix wasn't checked out: local changes are in the way."
    pub fn note(mut self, note: &str) -> Self {
        self.notes.push(note.to_string());
        self
    }
}

/// A task for [`respond`]: `key` ("TRACK-142") and `title`; the rest with the methods.
pub fn task(key: &str, title: &str) -> Task {
    Task {
        key: key.to_string(),
        title: title.to_string(),
        status: String::new(),
        url: None,
        markdown: String::new(),
        actions: Vec::new(),
        source: String::new(),
    }
}

/// One of the source's Claude actions on a task, for [`Task::action`]: `id` names it in a run,
/// `title` is the user's name for it ("Take to Work"); plan mode and taking the task to work first
/// with the methods.
pub fn action(id: &str, title: &str) -> TaskAction {
    TaskAction {
        id: id.to_string(),
        title: title.to_string(),
        plan: false,
        start: false,
    }
}

impl TaskAction {
    /// Claude starts in plan mode.
    pub fn plan(mut self) -> Self {
        self.plan = true;
        self
    }

    /// The task is taken to work first: its status, its branch, its tabs, its timer.
    pub fn start(mut self) -> Self {
        self.start = true;
        self
    }
}

impl Task {
    /// As the tracker names it: "In progress".
    pub fn status(mut self, status: &str) -> Self {
        self.status = status.to_string();
        self
    }

    pub fn url(mut self, url: &str) -> Self {
        self.url = Some(url.to_string());
        self
    }

    /// The task's text and comments for Claude: Markdown (`get` fills it, lists leave it empty).
    pub fn markdown(mut self, markdown: &str) -> Self {
        self.markdown = markdown.to_string();
        self
    }

    /// One of the source's Claude actions on it (`get` gives them, lists leave them out).
    pub fn action(mut self, action: TaskAction) -> Self {
        self.actions.push(action);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_task() {
        let task = task("TRACK-1", "Fix the sign-in")
            .status("In progress")
            .url("https://tracker.example/TRACK-1");
        assert_eq!(task.key, "TRACK-1");
        assert_eq!(task.status, "In progress");
        assert!(task.markdown.is_empty());
        assert!(task.actions.is_empty() && task.source.is_empty());
    }

    #[test]
    fn builds_actions_and_what_a_run_did() {
        let task = task("TRACK-1", "Fix")
            .action(action("work", "Take to Work").start())
            .action(action("plan", "Plan").plan());
        let flags: Vec<(&str, bool, bool)> = task
            .actions
            .iter()
            .map(|action| (action.id.as_str(), action.plan, action.start))
            .collect();
        assert_eq!(flags, [("work", false, true), ("plan", true, false)]);
        let started = opened("s-1").note("The branch wasn't checked out");
        assert_eq!(started.session, "s-1");
        assert_eq!(started.notes, ["The branch wasn't checked out"]);
    }
}
