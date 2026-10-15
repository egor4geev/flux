//! What is outside the window: the browser, the clipboard, the user's home folder (where the
//! folders of the manifest's `folders` permission are), Settings on the plugin's page; the clock
//! in the user's time zone (API 0.3).
//!
//! ```ignore
//! system::open_url("https://tracker.example.com/issue/FLUX-12")?;
//! system::copy_text("FLUX-12");
//! let config = std::fs::read_to_string(system::expand_home("~/.config/gh/hosts.yml"))?;
//! let starts = system::local_time(meeting.start).hm(); // "11:00"
//! ```

pub use crate::host::system::{
    copy_text, home_dir, open_settings, open_url, time_zone, utc_offset,
};

/// Now, in seconds since 1970-01-01 UTC.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// A moment in the user's local time: [`local_time`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i32,
    /// 1–12.
    pub month: u32,
    /// 1–31.
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// Monday — 0, Sunday — 6.
    pub weekday: u32,
    /// How far it is ahead of UTC, in seconds (summer time included).
    pub offset: i32,
}

/// `at` (seconds since 1970, UTC) in the user's time zone.
pub fn local_time(at: i64) -> LocalTime {
    LocalTime::at(at, utc_offset(at))
}

impl LocalTime {
    /// `at` with a known offset from UTC.
    pub fn at(at: i64, offset: i32) -> Self {
        let local = at + i64::from(offset);
        let days = local.div_euclid(86_400);
        let seconds = local.rem_euclid(86_400) as u32;
        let (year, month, day) = civil_from_days(days);
        LocalTime {
            year,
            month,
            day,
            hour: seconds / 3600,
            minute: seconds / 60 % 60,
            second: seconds % 60,
            weekday: (days + 3).rem_euclid(7) as u32,
            offset,
        }
    }

    /// "09:41".
    pub fn hm(&self) -> String {
        format!("{:02}:{:02}", self.hour, self.minute)
    }

    /// "2026-10-14".
    pub fn date(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// ISO 8601 with milliseconds and the offset, as Yandex Tracker takes a work log's start:
    /// "2026-10-14T10:00:00.000+0300".
    pub fn iso(&self) -> String {
        let sign = if self.offset < 0 { '-' } else { '+' };
        let offset = self.offset.unsigned_abs();
        format!(
            "{}T{:02}:{:02}:{:02}.000{sign}{:02}{:02}",
            self.date(),
            self.hour,
            self.minute,
            self.second,
            offset / 3600,
            offset / 60 % 60
        )
    }

    /// The local date's number of days since 1970-01-01: whether two moments fall on one day,
    /// or on the next.
    pub fn day_number(&self) -> i64 {
        days_from_civil(self.year, self.month, self.day)
    }
}

/// (year, month, day) of a day number (days since 1970-01-01), by Howard Hinnant's algorithm.
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year as i32, month, day)
}

/// The day number of a date: [`civil_from_days`] the other way.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let month = i64::from(month);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A path with `~` for the home folder made absolute: `~/.config/gh` → `/Users/me/.config/gh`.
/// Other paths stay as they are.
pub fn expand_home(path: &str) -> String {
    expand(&home_dir(), path)
}

fn expand(home: &str, path: &str) -> String {
    match path.strip_prefix('~') {
        Some("") => home.to_string(),
        Some(rest) if rest.starts_with('/') => format!("{}{rest}", home.trim_end_matches('/')),
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::expand;

    #[test]
    fn local_times() {
        use super::LocalTime;
        // 2026-10-14 07:00:00 UTC, a Wednesday: 10:00 in Moscow.
        let moscow = LocalTime::at(1_791_961_200, 3 * 3600);
        assert_eq!(moscow.date(), "2026-10-14");
        assert_eq!(moscow.hm(), "10:00");
        assert_eq!(moscow.weekday, 2);
        assert_eq!(moscow.iso(), "2026-10-14T10:00:00.000+0300");
        // Before the epoch, and a negative offset.
        let old = LocalTime::at(-1, 0);
        assert_eq!((old.year, old.month, old.day, old.hour), (1969, 12, 31, 23));
        let new_york = LocalTime::at(1_791_961_200, -4 * 3600 - 1800);
        assert_eq!(new_york.iso(), "2026-10-14T02:30:00.000-0430");
        // Leap days and day numbers both ways.
        let leap = LocalTime::at(951_782_400, 0);
        assert_eq!(leap.date(), "2000-02-29");
        assert_eq!(leap.day_number(), 951_782_400 / 86_400);
        assert_eq!(moscow.day_number() + 1, LocalTime::at(1_792_047_600, 3 * 3600).day_number());
    }

    #[test]
    fn expands_the_home_folder() {
        assert_eq!(expand("/Users/me", "~/.config/gh"), "/Users/me/.config/gh");
        assert_eq!(expand("/Users/me/", "~"), "/Users/me/");
        assert_eq!(expand("/Users/me", "~other/x"), "~other/x");
        assert_eq!(expand("/Users/me", "/etc/hosts"), "/etc/hosts");
    }
}
