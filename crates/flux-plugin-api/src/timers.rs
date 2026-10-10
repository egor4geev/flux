//! Timers: `Event::Timer(id)` after a delay or periodically, while the plugin runs — to poll a
//! service, to remind of a meeting. A periodic tick that still waits in the plugin's queue isn't
//! added again: a busy plugin doesn't fall behind.
//!
//! ```ignore
//! self.poll = Some(timers::every(Duration::from_secs(60)));
//! // …
//! Event::Timer(id) if Some(id) == self.poll => self.refresh(),
//! ```

use std::time::Duration;

use crate::host::timers as raw;

/// One `Event::Timer` after `delay`; returns the timer's id.
pub fn after(delay: Duration) -> u64 {
    raw::after(millis(delay))
}

/// An `Event::Timer` every `period` (at least 100 ms); returns the timer's id.
pub fn every(period: Duration) -> u64 {
    raw::every(millis(period))
}

/// Stops a timer.
pub fn cancel(timer: u64) {
    raw::cancel(timer)
}

fn millis(duration: Duration) -> u32 {
    duration.as_millis().min(u32::MAX as u128) as u32
}
