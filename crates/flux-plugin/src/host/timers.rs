//! `timers`: `timer` events after a delay or periodically, posted into the plugin's queue by a
//! thread of the plugin's own, started with its first timer (a periodic tick still waiting in the
//! queue isn't added again: the runtime coalesces it). A periodic timer that falls behind skips the
//! ticks it missed. The timers stop when the plugin does ([`Timers`]).

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::api::bindings::flux::plugin::timers;
use crate::api::events::Event;
use crate::log::Level;
use crate::runtime::{EventSender, State};

/// The shortest period of `every`.
const MIN_PERIOD: Duration = Duration::from_millis(100);

/// The plugin's timers: stopped when the plugin stops (dropped with its store).
#[derive(Default)]
pub(crate) struct Timers {
    /// Started with the first timer.
    scheduler: Option<Arc<Scheduler>>,
}

/// The timers and the thread that fires them.
struct Scheduler {
    schedule: Mutex<Schedule>,
    /// Wakes the thread: a timer added or cancelled, the plugin stopped.
    wake: Condvar,
}

#[derive(Default)]
struct Schedule {
    timers: HashMap<u64, Timer>,
    stopped: bool,
}

struct Timer {
    due: Instant,
    /// A periodic timer's period.
    every: Option<Duration>,
}

impl Timers {
    fn add(
        &mut self,
        id: u64,
        delay: Duration,
        every: Option<Duration>,
        events: &EventSender,
    ) -> bool {
        let scheduler = match &self.scheduler {
            Some(scheduler) => scheduler.clone(),
            None => match Scheduler::start(events.clone()) {
                Some(scheduler) => self.scheduler.insert(scheduler).clone(),
                None => return false,
            },
        };
        let timer = Timer {
            due: Instant::now() + delay,
            every,
        };
        scheduler.schedule.lock().unwrap().timers.insert(id, timer);
        scheduler.wake.notify_one();
        true
    }

    fn cancel(&mut self, id: u64) {
        if let Some(scheduler) = &self.scheduler {
            scheduler.schedule.lock().unwrap().timers.remove(&id);
            scheduler.wake.notify_one();
        }
    }
}

impl Drop for Timers {
    fn drop(&mut self) {
        if let Some(scheduler) = &self.scheduler {
            scheduler.schedule.lock().unwrap().stopped = true;
            scheduler.wake.notify_one();
        }
    }
}

impl Scheduler {
    /// The scheduler and its thread; none if the thread can't start.
    fn start(events: EventSender) -> Option<Arc<Scheduler>> {
        let scheduler = Arc::new(Scheduler {
            schedule: Mutex::new(Schedule::default()),
            wake: Condvar::new(),
        });
        let shared = scheduler.clone();
        thread::Builder::new()
            .name("plugin timers".into())
            .spawn(move || shared.run(&events))
            .ok()?;
        Some(scheduler)
    }

    /// Fires the timers that are due, then sleeps until the next one (or a change), until the
    /// plugin stops or its queue is gone.
    fn run(&self, events: &EventSender) {
        let mut schedule = self.schedule.lock().unwrap();
        loop {
            if schedule.stopped {
                return;
            }
            let now = Instant::now();
            let mut due: Vec<(Instant, u64)> = schedule
                .timers
                .iter()
                .filter(|(_, timer)| timer.due <= now)
                .map(|(id, timer)| (timer.due, *id))
                .collect();
            if due.is_empty() {
                let next = schedule.timers.values().map(|timer| timer.due).min();
                schedule = match next {
                    Some(next) => {
                        self.wake
                            .wait_timeout(schedule, next.saturating_duration_since(now))
                            .unwrap()
                            .0
                    }
                    None => self.wake.wait(schedule).unwrap(),
                };
                continue;
            }
            due.sort();
            for (_, id) in &due {
                let periodic = schedule.timers.get_mut(id).and_then(|timer| {
                    let every = timer.every?;
                    // The next tick; one that is late already is skipped.
                    timer.due = (timer.due + every).max(now + every / 2);
                    Some(())
                });
                if periodic.is_none() {
                    schedule.timers.remove(id);
                }
            }
            drop(schedule);
            for (_, id) in due {
                if !events.send(Event::Timer(id)) {
                    return;
                }
            }
            schedule = self.schedule.lock().unwrap();
        }
    }
}

impl timers::Host for State {
    fn after(&mut self, ms: u32) -> u64 {
        let id = self.host.next_id();
        let events = self.host.events.clone();
        if !self
            .host
            .timers
            .add(id, Duration::from_millis(ms.into()), None, &events)
        {
            self.host
                .log
                .write(Level::Error, "Couldn't start the timers' thread");
        }
        id
    }

    fn every(&mut self, ms: u32) -> u64 {
        let id = self.host.next_id();
        let period = Duration::from_millis(ms.into()).max(MIN_PERIOD);
        let events = self.host.events.clone();
        if !self.host.timers.add(id, period, Some(period), &events) {
            self.host
                .log
                .write(Level::Error, "Couldn't start the timers' thread");
        }
        id
    }

    fn cancel(&mut self, timer: u64) {
        self.host.timers.cancel(timer);
    }
}
