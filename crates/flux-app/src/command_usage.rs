//! What the command palette remembers (part 10.4): how often and how recently each command was
//! chosen in it, across all projects and windows — `~/Library/Application Support/flux/
//! command-usage.json` (`FLUX_COMMAND_USAGE_FILE` — another file; in a scenario without it, nothing
//! is read or written). With an empty query the palette starts with the frequent and recent
//! commands (as Recently Used of Find Action in JetBrains IDEs); with a query, usage adds to the
//! match.
//!
//! **Frecency.** Every choice adds 1 to the command's rank, and the rank halves every week the
//! command isn't chosen ([`HALF_LIFE_DAYS`]):
//!
//! ```text
//! frecency(now) = rank · 0.5^((now − last_used) / half-life)
//! a choice:       rank = frecency(now) + 1, last_used = now
//! ```
//!
//! So a command chosen five times yesterday comes before one chosen once an hour ago, and one chosen
//! twenty times two months ago has faded below both. The palette lists commands with a frecency of
//! at least [`MIN_RECENT`] in Recently Used (a single choice stays there about three weeks).
//!
//! **With a query** the match's own score is multiplied by `1 + MAX_BONUS · f / (f + 2)` (`f` — the
//! frecency): +10 % after one choice, approaching +30 % ([`MAX_BONUS`]) for a command in daily use.
//! Among matches of the same quality the used ones come first, and a match more than 30 % weaker
//! never overtakes a better one.
//!
//! Commands are keyed by their action's name (`editor::Save`); the plugins' commands by the plugin
//! and the command (`plugin:flux.todo/refresh`), their tool windows by the plugin and the window
//! (`plugin-window:flux.todo/todo`). The file keeps at most [`MAX_COMMANDS`] of them, the ones of
//! the highest frecency. A broken or missing file is an empty memory.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use flux_search::FuzzyMatch;
use gpui::{App, Global};
use serde_json::{Map, Value, json};

/// The rank halves in this many days without a choice.
pub const HALF_LIFE_DAYS: f64 = 7.;
/// The least frecency of a command in Recently Used.
pub const MIN_RECENT: f64 = 0.1;
/// How many commands Recently Used shows.
pub const RECENT_LIMIT: usize = 8;
/// The most a command's usage multiplies its match's score by, minus one: +30 %.
pub const MAX_BONUS: f64 = 0.3;
/// How many commands the file remembers.
pub const MAX_COMMANDS: usize = 500;

const SECONDS_PER_DAY: f64 = 86_400.;

/// One command's usage.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Usage {
    /// How many times the command was chosen, ever.
    pub count: u32,
    /// When it was chosen last: seconds since the Unix epoch.
    pub last_used: u64,
    /// The frecency as of `last_used`.
    pub rank: f64,
}

impl Usage {
    /// The frecency at `now` (seconds since the Unix epoch).
    pub fn frecency(&self, now: u64) -> f64 {
        let age_days = now.saturating_sub(self.last_used) as f64 / SECONDS_PER_DAY;
        self.rank * 0.5_f64.powf(age_days / HALF_LIFE_DAYS)
    }

    /// The usage after one more choice at `now`.
    fn chosen(self, now: u64) -> Self {
        Usage {
            count: self.count.saturating_add(1),
            last_used: now.max(self.last_used),
            rank: self.frecency(now) + 1.,
        }
    }
}

/// The palette's memory, shared by every window.
#[derive(Debug, Default)]
pub struct CommandUsage {
    commands: HashMap<String, Usage>,
    /// Where the memory is saved; `None` — kept in memory only (a scenario).
    path: Option<PathBuf>,
}

impl Global for CommandUsage {}

impl CommandUsage {
    /// One more choice of the command `key` at `now`; the least used are forgotten past
    /// [`MAX_COMMANDS`].
    fn record(&mut self, key: &str, now: u64) {
        let usage = self.commands.get(key).copied().unwrap_or(Usage {
            count: 0,
            last_used: now,
            rank: 0.,
        });
        self.commands.insert(key.to_string(), usage.chosen(now));
        prune(&mut self.commands, now);
    }

    fn save(&self) {
        if let Some(path) = &self.path
            && let Err(error) = write(path, &to_json(&self.commands))
        {
            eprintln!("flux: can't save {}: {error}", path.display());
        }
    }
}

pub fn init(cx: &mut App) {
    let path = store_path_for(
        std::env::var_os("FLUX_COMMAND_USAGE_FILE"),
        std::env::var_os("FLUX_SCENARIO").is_some(),
        flux_platform::paths::home().map(std::ffi::OsString::from),
    );
    let commands = path.as_deref().map(load).unwrap_or_default();
    cx.set_global(CommandUsage { commands, path });
}

/// The command `key` was chosen in the palette: remembered and saved.
pub fn record(key: &str, cx: &mut App) {
    let store = cx.default_global::<CommandUsage>();
    store.record(key, now());
    store.save();
}

/// The frecency of every remembered command, now: what the palette ranks by while it is open.
pub fn frecencies(cx: &App) -> HashMap<String, f64> {
    let now = now();
    cx.try_global::<CommandUsage>()
        .map(|store| {
            store
                .commands
                .iter()
                .map(|(key, usage)| (key.clone(), usage.frecency(now)))
                .collect()
        })
        .unwrap_or_default()
}

/// The match's score with its command's usage: multiplied by at most `1 + MAX_BONUS`.
pub fn boosted_score(score: u32, frecency: f64) -> f64 {
    let frecency = frecency.max(0.);
    f64::from(score) * (1. + MAX_BONUS * frecency / (frecency + 2.))
}

/// Fuzzy matches ranked with usage: by [`boosted_score`], ties in the order they came in (the
/// matcher's, which keeps the list's order among equal scores).
pub fn rank_matches(
    mut matches: Vec<FuzzyMatch>,
    frecency: impl Fn(usize) -> f64,
) -> Vec<FuzzyMatch> {
    let mut keyed: Vec<(f64, FuzzyMatch)> = matches
        .drain(..)
        .map(|found| (boosted_score(found.score, frecency(found.index)), found))
        .collect();
    keyed.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal));
    keyed.into_iter().map(|(_, found)| found).collect()
}

/// The items of Recently Used among `count` items: the indices with a frecency of at least
/// [`MIN_RECENT`], the most used first (ties — in the list's order), at most `limit`.
pub fn recent(count: usize, frecency: impl Fn(usize) -> f64, limit: usize) -> Vec<usize> {
    let mut used: Vec<(usize, f64)> = (0..count)
        .map(|index| (index, frecency(index)))
        .filter(|(_, frecency)| *frecency >= MIN_RECENT)
        .collect();
    used.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
    used.into_iter()
        .take(limit)
        .map(|(index, _)| index)
        .collect()
}

/// Past [`MAX_COMMANDS`], the commands of the least frecency are forgotten.
fn prune(commands: &mut HashMap<String, Usage>, now: u64) {
    if commands.len() <= MAX_COMMANDS {
        return;
    }
    let mut by_frecency: Vec<(String, f64)> = commands
        .iter()
        .map(|(key, usage)| (key.clone(), usage.frecency(now)))
        .collect();
    by_frecency.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
    for (key, _) in by_frecency.into_iter().skip(MAX_COMMANDS) {
        commands.remove(&key);
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Where the memory is stored: an explicit file, otherwise (outside a scenario) Application
/// Support.
fn store_path_for(
    file: Option<OsString>,
    scenario: bool,
    home: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(file) = file.filter(|file| !file.is_empty()) {
        return Some(file.into());
    }
    if scenario {
        return None;
    }
    let home = PathBuf::from(home?);
    Some(flux_platform::paths::support_dir_in(&home).join("command-usage.json"))
}

fn load(path: &Path) -> HashMap<String, Usage> {
    match fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
        Err(error) => {
            eprintln!("flux: {}: {error}", path.display());
            HashMap::new()
        }
    }
}

/// File → memory; entries that aren't well-formed are skipped, a broken file is an empty memory.
fn parse(text: &str) -> HashMap<String, Usage> {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return HashMap::new();
    };
    let Some(commands) = value["commands"].as_object() else {
        return HashMap::new();
    };
    commands
        .iter()
        .filter_map(|(key, usage)| {
            let count = u32::try_from(usage["count"].as_u64()?).ok()?;
            let last_used = usage["last_used"].as_u64()?;
            let rank = usage["rank"]
                .as_f64()
                .filter(|rank| rank.is_finite() && *rank >= 0.)?;
            (!key.is_empty()).then(|| {
                (
                    key.clone(),
                    Usage {
                        count,
                        last_used,
                        rank,
                    },
                )
            })
        })
        .collect()
}

fn to_json(commands: &HashMap<String, Usage>) -> String {
    // Sorted by key: the file changes only where the usage did.
    let mut keys: Vec<&String> = commands.keys().collect();
    keys.sort();
    let entries: Map<String, Value> = keys
        .into_iter()
        .map(|key| {
            let usage = &commands[key];
            // The rank to a thousandth: enough to rank by, and the file stays readable.
            let rank = (usage.rank * 1000.).round() / 1000.;
            (
                key.clone(),
                json!({ "count": usage.count, "last_used": usage.last_used, "rank": rank }),
            )
        })
        .collect();
    let value = json!({ "commands": entries });
    serde_json::to_string_pretty(&value).unwrap_or_default() + "\n"
}

/// Through a temporary file and a `rename`: the file is never left half-written.
fn write(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension("json.flux-tmp");
    fs::write(&temp, text)?;
    fs::rename(&temp, path).inspect_err(|_| {
        fs::remove_file(&temp).ok();
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;
    const NOW: u64 = 1_760_000_000;

    fn found(index: usize, score: u32) -> FuzzyMatch {
        FuzzyMatch {
            index,
            score,
            positions: Vec::new(),
        }
    }

    fn indices(matches: &[FuzzyMatch]) -> Vec<usize> {
        matches.iter().map(|found| found.index).collect()
    }

    /// Each test has its own temporary directory: tests run in parallel.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "flux-command-usage-test-{}-{name}",
            std::process::id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn rank_grows_with_choices_and_halves_every_week() {
        let mut store = CommandUsage::default();
        store.record("editor::Save", NOW);
        store.record("editor::Save", NOW);
        let usage = store.commands["editor::Save"];
        assert_eq!(usage.count, 2);
        assert_eq!(usage.last_used, NOW);
        assert!((usage.frecency(NOW) - 2.).abs() < 1e-9);
        assert!((usage.frecency(NOW + 7 * DAY) - 1.).abs() < 1e-9);
        assert!((usage.frecency(NOW + 14 * DAY) - 0.5).abs() < 1e-9);
        // A choice a week later adds to what is left.
        store.record("editor::Save", NOW + 7 * DAY);
        let usage = store.commands["editor::Save"];
        assert_eq!(usage.count, 3);
        assert!((usage.frecency(NOW + 7 * DAY) - 2.).abs() < 1e-9);
        // A clock that went back doesn't make the command older.
        store.record("editor::Save", NOW);
        assert_eq!(store.commands["editor::Save"].last_used, NOW + 7 * DAY);
    }

    #[test]
    fn frequent_beats_recent_until_it_fades() {
        let mut store = CommandUsage::default();
        for _ in 0..5 {
            store.record("frequent", NOW - DAY);
        }
        store.record("recent", NOW - 3600);
        for _ in 0..20 {
            store.record("faded", NOW - 60 * DAY);
        }
        let frecency = |key: &str| store.commands[key].frecency(NOW);
        assert!(frecency("frequent") > frecency("recent"));
        assert!(frecency("recent") > frecency("faded"));
    }

    #[test]
    fn recently_used_are_ranked_and_limited() {
        let frecencies = [0., 2.5, MIN_RECENT / 2., 0.4, 2.5, 7., MIN_RECENT];
        let frecency = |index: usize| frecencies[index];
        // Ties stay in the list's order; below MIN_RECENT is left out.
        assert_eq!(recent(frecencies.len(), frecency, 8), [5, 1, 4, 3, 6]);
        assert_eq!(recent(frecencies.len(), frecency, 2), [5, 1]);
        assert!(recent(3, |_| 0., 8).is_empty());
    }

    #[test]
    fn usage_bonus_is_bounded() {
        assert_eq!(boosted_score(100, 0.), 100.);
        // One choice: +10 %.
        assert!((boosted_score(100, 1.) - 110.).abs() < 1e-9);
        // Never more than +30 %, however much the command is used.
        for frecency in [5., 50., 1e6, f64::MAX] {
            let boosted = boosted_score(100, frecency);
            assert!(boosted > 100. && boosted <= 130., "{frecency}: {boosted}");
        }
        assert_eq!(boosted_score(100, -3.), 100.);
    }

    #[test]
    fn a_used_command_rises_among_matches_of_the_same_quality() {
        // The matcher's order: by score, ties in the list's order.
        let matches = vec![found(0, 120), found(1, 120), found(2, 100), found(3, 60)];
        let usage = [0., 0., 0., 0.];
        assert_eq!(
            indices(&rank_matches(matches.clone(), |index| usage[index])),
            [0, 1, 2, 3]
        );
        // A tie goes to the used command.
        let usage = [0., 1., 0., 0.];
        assert_eq!(
            indices(&rank_matches(matches.clone(), |index| usage[index])),
            [1, 0, 2, 3]
        );
        // A heavily used, slightly weaker match rises above better ones…
        let usage = [0., 0., 30., 0.];
        assert_eq!(
            indices(&rank_matches(matches.clone(), |index| usage[index])),
            [2, 0, 1, 3]
        );
        // …but a poor one never beats a match more than 30 % better.
        let usage = [0., 0., 0., f64::MAX];
        assert_eq!(
            indices(&rank_matches(matches, |index| usage[index])),
            [0, 1, 2, 3]
        );
    }

    #[test]
    fn memory_survives_the_file() {
        let dir = temp_dir("round-trip");
        let path = dir.join("nested/command-usage.json");
        let mut store = CommandUsage {
            commands: HashMap::new(),
            path: Some(path.clone()),
        };
        store.record("editor::Save", NOW);
        store.record("plugin:flux.todo/refresh", NOW - DAY);
        store.record("plugin:flux.todo/refresh", NOW);
        store.save();
        let loaded = load(&path);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded["editor::Save"], store.commands["editor::Save"]);
        let todo = loaded["plugin:flux.todo/refresh"];
        assert_eq!((todo.count, todo.last_used), (2, NOW));
        let rank = store.commands["plugin:flux.todo/refresh"].rank;
        assert!((todo.rank - rank).abs() < 1e-3);
        // No temporary file is left next to it.
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["command-usage.json"]);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn broken_or_missing_files_are_an_empty_memory() {
        assert!(parse("not json").is_empty());
        assert!(parse("{}").is_empty());
        assert!(parse(r#"{"commands": []}"#).is_empty());
        // Entries that aren't well-formed are skipped, the rest stay.
        let parsed = parse(
            r#"{"commands": {
                "good": {"count": 2, "last_used": 1760000000, "rank": 1.5},
                "negative": {"count": 1, "last_used": 1760000000, "rank": -1},
                "text": {"count": "2", "last_used": 1760000000, "rank": 1},
                "": {"count": 1, "last_used": 1760000000, "rank": 1}
            }}"#,
        );
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed["good"].count, 2);
        assert!(load(&temp_dir("missing").join("none.json")).is_empty());
    }

    #[test]
    fn the_least_used_are_forgotten_past_the_limit() {
        let mut store = CommandUsage::default();
        for n in 0..MAX_COMMANDS {
            store.record(&format!("old::{n}"), NOW - 30 * DAY);
        }
        store.record("new::Fresh", NOW);
        assert_eq!(store.commands.len(), MAX_COMMANDS);
        assert!(store.commands.contains_key("new::Fresh"));
    }

    #[test]
    fn scenarios_without_a_file_keep_it_in_memory() {
        let home = Some(OsString::from("/Users/me"));
        assert_eq!(
            store_path_for(None, false, home.clone()),
            Some(crate::settings::support_file(
                "/Users/me",
                "command-usage.json"
            ))
        );
        assert_eq!(store_path_for(None, true, home.clone()), None);
        assert_eq!(
            store_path_for(Some("/tmp/usage.json".into()), true, home.clone()),
            Some(PathBuf::from("/tmp/usage.json"))
        );
        assert_eq!(store_path_for(Some(OsString::new()), true, home), None);
        assert_eq!(store_path_for(None, false, None), None);
        // In memory, a choice is remembered and nothing is written.
        let mut store = CommandUsage::default();
        store.record("editor::Save", NOW);
        store.save();
        assert_eq!(store.commands["editor::Save"].count, 1);
    }
}
