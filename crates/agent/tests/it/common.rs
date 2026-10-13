//! What the end-to-end tests share.

use std::{
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};

/// Held for the whole test: the tests set the process's environment (the
/// agent's socket and folders), which the others would see.
pub fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    // a test that failed still lets the next one in
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let start = Instant::now();
    while !check() {
        assert!(start.elapsed() < Duration::from_secs(5), "never arrived: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
