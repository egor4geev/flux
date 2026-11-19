//! The user's tasks between plugins (API 0.6). A tracker's plugin gives them — `task-source =
//! true` in its manifest — and answers each `Event::TaskQuery` with [`respond`]; another plugin
//! (the `tasks` permission) asks Flux with [`mine`], [`search`] or [`get`], which wait for every
//! source (up to ten seconds) and merge what they gave. The query goes to the source's service
//! when it has one, else to its instance in a window; never to the instance that asks.
//!
//! ```ignore
//! // The tracker:
//! fn on_event(&mut self, event: Event) {
//!     if let Event::TaskQuery(query) = event {
//!         let found = match query.query {
//!             Query::Mine => self.my_tasks(),
//!             Query::Search(text) => self.search(&text),
//!             Query::Get(key) => self.task(&key),
//!         };
//!         tasks::respond(query.id, found);
//!     }
//! }
//!
//! // The bot:
//! for task in tasks::mine()? {
//!     // task.key, task.title, task.status
//! }
//! ```

pub use crate::host::tasks::{Query, Task, TaskQuery};

use crate::host::tasks as raw;

/// The user's open tasks, from every source.
pub fn mine() -> Result<Vec<Task>, String> {
    raw::find(&Query::Mine)
}

/// The tasks the user's words find.
pub fn search(text: &str) -> Result<Vec<Task>, String> {
    raw::find(&Query::Search(text.to_string()))
}

/// One task by its key, with its text and comments (`markdown`); none when no source knows it.
pub fn get(key: &str) -> Result<Option<Task>, String> {
    Ok(raw::find(&Query::Get(key.to_string()))?.into_iter().next())
}

/// A source's answer to `Event::TaskQuery` (by its id).
pub fn respond(query: u64, tasks: Result<Vec<Task>, String>) {
    match &tasks {
        Ok(tasks) => raw::respond(query, Ok(tasks)),
        Err(error) => raw::respond(query, Err(error)),
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
    }
}
