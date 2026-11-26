//! Wakes the compose loop when there's something to show.

use std::sync::{Condvar, Mutex};
use std::time::Duration;

use anyhow::Result;

pub struct Doorbell {
    rung: Mutex<bool>,
    changed: Condvar,
}

impl Doorbell {
    pub fn new() -> Result<Self> {
        Ok(Self { rung: Mutex::new(false), changed: Condvar::new() })
    }

    pub fn ring(&self) {
        *self.rung.lock().expect("doorbell poisoned") = true;
        self.changed.notify_one();
    }

    /// Waits for a ring, or `timeout`, and clears it.
    pub fn wait(&self, timeout: Duration) {
        let rung = self.rung.lock().expect("doorbell poisoned");
        let (mut rung, _) = self.changed.wait_timeout_while(rung, timeout, |rung| !*rung).expect("doorbell poisoned");
        *rung = false;
    }
}
