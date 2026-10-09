//! Values shared by the protocol and the session: modes, models, limits, the user's input.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// How Claude asks before acting (`--permission-mode`, `set_permission_mode`; ⇧⇥ in the CLI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum PermissionMode {
    /// Asks before edits, commands that change things, the web and MCP tools.
    #[default]
    Default,
    /// Edits and file commands in the project go through without asking.
    AcceptEdits,
    /// Read-only exploration; the plan is approved before anything changes.
    Plan,
    /// Whatever would ask is refused.
    DontAsk,
    /// A classifier decides (not every model has it).
    Auto,
    /// Nothing is checked; only when `claude` was started allowing it.
    BypassPermissions,
}

impl PermissionMode {
    pub const ALL: [PermissionMode; 6] = [
        PermissionMode::Default,
        PermissionMode::AcceptEdits,
        PermissionMode::Plan,
        PermissionMode::DontAsk,
        PermissionMode::Auto,
        PermissionMode::BypassPermissions,
    ];

    pub fn wire(self) -> &'static str {
        match self {
            PermissionMode::Default => "default",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::Plan => "plan",
            PermissionMode::DontAsk => "dontAsk",
            PermissionMode::Auto => "auto",
            PermissionMode::BypassPermissions => "bypassPermissions",
        }
    }

    /// From the wire name; "manual" is the old name of the default mode.
    pub fn from_wire(name: &str) -> Option<Self> {
        Some(match name {
            "default" | "manual" => PermissionMode::Default,
            "acceptEdits" => PermissionMode::AcceptEdits,
            "plan" => PermissionMode::Plan,
            "dontAsk" => PermissionMode::DontAsk,
            "auto" => PermissionMode::Auto,
            "bypassPermissions" => PermissionMode::BypassPermissions,
            _ => return None,
        })
    }
}

/// How hard the model thinks (`--effort`; not every model supports every level).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Effort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Effort {
    pub const ALL: [Effort; 5] = [
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::XHigh,
        Effort::Max,
    ];

    pub fn wire(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        }
    }

    pub fn from_wire(name: &str) -> Option<Self> {
        Effort::ALL.into_iter().find(|effort| effort.wire() == name)
    }
}

/// A model the account can use (`list_models`, `initialize`).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    /// What `--model` / `set_model` takes: an alias ("opus") or an id.
    pub value: String,
    pub display_name: String,
    pub description: String,
    /// The model id an alias resolves to ("default" → "claude-opus-5-5"): matches `init.model`.
    pub resolved: Option<String>,
    /// The effort levels it supports; empty — no effort control.
    pub efforts: Vec<Effort>,
    /// Listed but not usable with this CLI ("Update Claude Code to use this model").
    pub disabled: bool,
}

impl ModelInfo {
    pub fn from_json(value: &Value) -> Option<Self> {
        Some(ModelInfo {
            value: value["value"].as_str()?.to_string(),
            display_name: value["displayName"]
                .as_str()
                .or_else(|| value["value"].as_str())?
                .to_string(),
            description: value["description"].as_str().unwrap_or("").to_string(),
            resolved: value["resolvedModel"]
                .as_str()
                .filter(|id| !id.is_empty())
                .map(str::to_string),
            efforts: value["supportedEffortLevels"]
                .as_array()
                .map(|levels| {
                    levels
                        .iter()
                        .filter_map(|level| Effort::from_wire(level.as_str()?))
                        .collect()
                })
                .unwrap_or_default(),
            disabled: value["disabled"].as_bool().unwrap_or(false),
        })
    }
}

/// A slash command the CLI knows (`initialize`, `commands_changed`): built-in, custom, a skill.
#[derive(Debug, Clone, PartialEq)]
pub struct SlashCommand {
    /// Without the slash: "compact", "anthropic-skills:docx".
    pub name: String,
    pub description: String,
    /// "<file>", "[focus]".
    pub argument_hint: Option<String>,
    pub aliases: Vec<String>,
    pub builtin: bool,
}

impl SlashCommand {
    pub fn from_json(value: &Value) -> Option<Self> {
        Some(SlashCommand {
            name: value["name"].as_str()?.to_string(),
            description: value["description"].as_str().unwrap_or("").to_string(),
            argument_hint: value["argumentHint"]
                .as_str()
                .filter(|hint| !hint.is_empty())
                .map(str::to_string),
            aliases: value["aliases"]
                .as_array()
                .map(|aliases| {
                    aliases
                        .iter()
                        .filter_map(|alias| Some(alias.as_str()?.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            builtin: value["builtin"].as_bool().unwrap_or(false),
        })
    }
}

/// The subscription's usage windows (`rate_limit_event`): 5 hours and 7 days.
#[derive(Debug, Clone, PartialEq)]
pub struct RateLimits {
    pub status: LimitStatus,
    pub five_hour: Option<LimitWindow>,
    pub seven_day: Option<LimitWindow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LimitStatus {
    #[default]
    Allowed,
    /// Close to a limit.
    Warning,
    /// A limit is reached: requests fail until it resets.
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LimitWindow {
    /// 0.0–1.0.
    pub utilization: f32,
    pub resets_at: SystemTime,
}

impl RateLimits {
    /// From `rate_limit_event.rate_limit_info`: fractions and epoch seconds.
    pub fn from_event(info: &Value) -> Self {
        let window = |name: &str| {
            let window = &info["unifiedWindows"][name];
            Some(LimitWindow {
                utilization: window["utilization"].as_f64()? as f32,
                resets_at: UNIX_EPOCH + Duration::from_secs(window["resetsAt"].as_u64()?),
            })
        };
        RateLimits {
            status: match info["status"].as_str() {
                Some("allowed_warning") => LimitStatus::Warning,
                Some("rejected") => LimitStatus::Rejected,
                _ => LimitStatus::Allowed,
            },
            five_hour: window("five_hour"),
            seven_day: window("seven_day"),
        }
    }
}

impl RateLimits {
    /// From the answer of `get_usage`: percents (0–100) and ISO 8601 reset times; `None` while the
    /// CLI hasn't fetched them yet (right after it starts — ask again a little later).
    pub fn from_usage(response: &Value) -> Option<Self> {
        let limits = response
            .get("rate_limits")
            .filter(|limits| !limits.is_null())?;
        let window = |name: &str| {
            let window = &limits[name];
            Some(LimitWindow {
                utilization: (window["utilization"].as_f64()? / 100.) as f32,
                resets_at: parse_time(window["resets_at"].as_str()?)?,
            })
        };
        let (five_hour, seven_day) = (window("five_hour"), window("seven_day"));
        if five_hour.is_none() && seven_day.is_none() {
            return None;
        }
        let full = [&five_hour, &seven_day]
            .iter()
            .any(|window| window.is_some_and(|window| window.utilization >= 1.));
        Some(RateLimits {
            status: if full {
                LimitStatus::Rejected
            } else {
                LimitStatus::Allowed
            },
            five_hour,
            seven_day,
        })
    }
}

/// An ISO 8601 / RFC 3339 time: "2026-10-10T01:10:00Z", "2026-10-09T22:10:00.123+03:00".
pub(crate) fn parse_time(text: &str) -> Option<SystemTime> {
    let (date, rest) = text.split_once('T')?;
    let mut date = date.split('-').map(|part| part.parse::<i64>().ok());
    let (year, month, day) = (date.next()??, date.next()??, date.next()??);
    let time_end = rest.find(['Z', 'z', '+', '-']).unwrap_or(rest.len());
    let (time, zone) = rest.split_at(time_end);
    let mut clock = time.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: f64 = clock.next().unwrap_or("0").parse().ok()?;
    let offset = match zone.chars().next() {
        None | Some('Z') | Some('z') => 0,
        Some(sign) => {
            let (hours, minutes) = zone[1..].split_once(':').unwrap_or((&zone[1..], "0"));
            let minutes = hours.parse::<i64>().ok()? * 60 + minutes.parse::<i64>().ok()?;
            if sign == '-' {
                -minutes * 60
            } else {
                minutes * 60
            }
        }
    };
    // Days from the civil date (Howard Hinnant's algorithm).
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 - offset;
    let time = seconds as f64 + second;
    (time >= 0.).then(|| UNIX_EPOCH + Duration::from_secs_f64(time))
}

/// How full the context window is (`get_context_usage`, a turn's result).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContextUsage {
    pub used: u64,
    pub max: u64,
}

impl ContextUsage {
    /// 0.0–1.0.
    pub fn fraction(&self) -> f32 {
        if self.max == 0 {
            0.
        } else {
            (self.used as f32 / self.max as f32).clamp(0., 1.)
        }
    }
}

/// A command or an agent running in the background (`background_tasks_changed`).
#[derive(Debug, Clone, PartialEq)]
pub struct BackgroundTask {
    pub task_id: String,
    /// "local_bash", "local_agent".
    pub task_type: String,
    pub description: String,
}

/// What the user sends: text (slash commands and `@path` mentions are plain text — the CLI
/// expands them) and pictures.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UserInput {
    pub text: String,
    pub images: Vec<ImageAttachment>,
    /// While a turn runs: `None` — after it (queued); `Now` — stops it and runs at once.
    pub priority: Option<Priority>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImageAttachment {
    /// "image/png", "image/jpeg", "image/gif", "image/webp".
    pub media_type: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Now,
    Next,
    Later,
}

impl Priority {
    pub fn wire(self) -> &'static str {
        match self {
            Priority::Now => "now",
            Priority::Next => "next",
            Priority::Later => "later",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn times_parse_with_zones_and_fractions() {
        let at = |text| {
            parse_time(text)
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
        };
        assert_eq!(at("1970-01-01T00:00:00Z"), 0);
        assert_eq!(at("2026-10-10T01:10:00Z"), 1_791_594_600);
        assert_eq!(at("2026-10-10T04:10:00.5+03:00"), 1_791_594_600);
        assert_eq!(at("2026-10-09T20:10:00-05:00"), 1_791_594_600);
        assert!(parse_time("yesterday").is_none());
    }

    #[test]
    fn usage_limits_come_in_percents() {
        let response = json!({ "rate_limits": {
            "five_hour": { "utilization": 12, "resets_at": "2026-10-10T01:10:00Z" },
            "seven_day": { "utilization": 49.5, "resets_at": "2026-10-14T00:00:00Z" },
        }});
        let limits = RateLimits::from_usage(&response).unwrap();
        assert!((limits.five_hour.unwrap().utilization - 0.12).abs() < 1e-6);
        assert!((limits.seven_day.unwrap().utilization - 0.495).abs() < 1e-6);
        assert_eq!(limits.status, LimitStatus::Allowed);
        assert!(RateLimits::from_usage(&json!({ "rate_limits": null })).is_none());
    }
}
