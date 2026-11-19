//! One sound at a time across Flux: a voice message, a video or a round message with sound.
//! Whatever starts playing with sound takes it ([`take`]); the one that had it pauses — its
//! player's `on_change` tells the app. A video without sound (a GIF, a round message in the feed)
//! takes nothing.

use std::sync::{Arc, Mutex, Weak};

/// A player that can be paused by another sound.
pub trait Sound: Send + Sync {
    /// Another sound took over: pause (and tell the app through `on_change`).
    fn pause_for_another(&self);
}

static CURRENT: Mutex<Option<Weak<dyn Sound>>> = Mutex::new(None);

/// `sound` takes the sound: the one that had it (if another, still alive) pauses.
pub fn take(sound: &Arc<dyn Sound>) {
    let previous = CURRENT
        .lock()
        .unwrap()
        .replace(Arc::downgrade(sound))
        .and_then(|previous| previous.upgrade());
    if let Some(previous) = previous
        && !Arc::ptr_eq(&previous, sound)
    {
        previous.pause_for_another();
    }
}

/// `sound` let it go (paused, ended, dropped): nobody has it if it had.
pub fn release(sound: &Arc<dyn Sound>) {
    let mut current = CURRENT.lock().unwrap();
    if current
        .as_ref()
        .and_then(Weak::upgrade)
        .is_some_and(|current| Arc::ptr_eq(&current, sound))
    {
        *current = None;
    }
}

/// Pauses whatever plays with sound: Flux quits, the window with it closes.
pub fn pause_all() {
    let current = CURRENT
        .lock()
        .unwrap()
        .take()
        .and_then(|sound| sound.upgrade());
    if let Some(current) = current {
        current.pause_for_another();
    }
}
