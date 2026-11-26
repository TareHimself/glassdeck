use std::sync::{Arc, Mutex};

use crate::platform::{Doorbell, Picture};

pub enum Timing {
    Live { capture_us: u64, received_us: u64, decoded_us: u64 },
    Paced { late_us: u64 },
}

pub struct ShownFrame {
    pub picture: Picture,
    pub timing: Timing,
}

/// Holds only the newest frame, so a slow display skips frames instead of falling behind.
pub struct Mailbox {
    slot: Mutex<(Option<ShownFrame>, u32)>,
    doorbell: Arc<Doorbell>,
}

impl Mailbox {
    pub fn new(doorbell: Arc<Doorbell>) -> Self {
        Self { slot: Mutex::new((None, 0)), doorbell }
    }

    pub fn put(&self, frame: ShownFrame) {
        let mut slot = self.slot.lock().expect("mailbox poisoned");
        if slot.0.replace(frame).is_some() {
            slot.1 += 1;
        }
        drop(slot);
        self.doorbell.ring();
    }

    pub fn take(&self) -> (Option<ShownFrame>, u32) {
        let mut slot = self.slot.lock().expect("mailbox poisoned");
        (slot.0.take(), std::mem::take(&mut slot.1))
    }
}
