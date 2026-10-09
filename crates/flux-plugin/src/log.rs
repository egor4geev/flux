//! A plugin's log: the last lines in memory (Settings → Plugins → the plugin → Log) and a file on
//! disk (`paths::logs_dir()/<id>.log`). The plugin writes through the `log` interface and its
//! stdout and stderr; Flux adds what happens to the plugin (started, stopped, the build of a
//! plugin under development).

use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

pub use crate::api::log::Level;

/// The lines kept in memory.
pub const MAX_LINES: usize = 2000;
/// A log file larger than this is moved to `<id>.log.old` when the plugin starts.
const MAX_FILE_BYTES: u64 = 1 << 20;

#[derive(Debug, Clone, PartialEq)]
pub struct LogLine {
    pub time: SystemTime,
    pub level: Level,
    pub text: String,
}

#[derive(Clone, Default)]
pub struct PluginLog(Arc<Mutex<Inner>>);

#[derive(Default)]
struct Inner {
    lines: VecDeque<LogLine>,
    file: Option<PathBuf>,
}

impl std::fmt::Debug for PluginLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginLog").finish_non_exhaustive()
    }
}

impl PluginLog {
    /// The log of the plugin `id`, with its file.
    pub fn open(id: &str) -> PluginLog {
        let dir = crate::paths::logs_dir();
        let file = dir.join(format!("{id}.log"));
        if std::fs::metadata(&file).is_ok_and(|meta| meta.len() > MAX_FILE_BYTES) {
            let _ = std::fs::rename(&file, dir.join(format!("{id}.log.old")));
        }
        let _ = std::fs::create_dir_all(&dir);
        PluginLog(Arc::new(Mutex::new(Inner {
            lines: VecDeque::new(),
            file: Some(file),
        })))
    }

    /// A log in memory only (tests).
    pub fn memory() -> PluginLog {
        PluginLog::default()
    }

    /// Adds a line (several for a text with newlines).
    pub fn write(&self, level: Level, text: &str) {
        let time = SystemTime::now();
        let mut inner = self.0.lock().unwrap();
        for line in text.lines() {
            if inner.lines.len() == MAX_LINES {
                inner.lines.pop_front();
            }
            inner.lines.push_back(LogLine {
                time,
                level,
                text: line.to_string(),
            });
        }
        if let Some(file) = &inner.file
            && let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(file)
        {
            for line in text.lines() {
                let _ = writeln!(file, "{} {:?} {line}", stamp(time), level);
            }
        }
    }

    /// The lines in memory, oldest first.
    pub fn lines(&self) -> Vec<LogLine> {
        self.0.lock().unwrap().lines.iter().cloned().collect()
    }

    /// The file on disk, if the log has one.
    pub fn path(&self) -> Option<PathBuf> {
        self.0.lock().unwrap().file.clone()
    }

    /// Forgets the lines in memory (the file stays).
    pub fn clear(&self) {
        self.0.lock().unwrap().lines.clear();
    }
}

/// Seconds since the epoch with milliseconds: a log line's time in the file.
fn stamp(time: SystemTime) -> String {
    let since = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}", since.as_secs(), since.subsec_millis())
}
