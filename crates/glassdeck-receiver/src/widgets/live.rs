//! A widget instance's live resources: what its host half sent at run time, kept within a budget.

use std::cell::Cell;
use std::collections::HashMap;

use anyhow::{Result, ensure};
use glassdeck_luau::{LiveName, MAX_LIVE_RESOURCE_BYTES};

/// Per widget instance. Over it, the least recently drawn resources go first.
pub const LIVE_BUDGET_BYTES: usize = 4 * 1024 * 1024;

struct Entry {
    bytes: Vec<u8>,
    drawn: Cell<u64>,
}

#[derive(Default)]
pub struct LiveCache {
    entries: HashMap<LiveName, Entry>,
    clock: Cell<u64>,
}

impl LiveCache {
    /// Stores `bytes` under `name`, or removes it when `bytes` is empty. Reports what it dropped to
    /// stay within budget.
    pub fn put(&mut self, name: LiveName, bytes: Vec<u8>) -> Result<Vec<LiveName>> {
        ensure!(bytes.len() <= MAX_LIVE_RESOURCE_BYTES, "live resource {} is {} bytes, over the {MAX_LIVE_RESOURCE_BYTES} limit", name.as_str(), bytes.len());
        if bytes.is_empty() {
            self.entries.remove(&name);
            return Ok(Vec::new());
        }
        self.entries.insert(name.clone(), Entry { bytes, drawn: Cell::new(self.tick()) });
        let mut dropped = Vec::new();
        while self.size() > LIVE_BUDGET_BYTES {
            let oldest = self.entries.iter().filter(|(n, _)| **n != name).min_by_key(|(_, e)| e.drawn.get()).map(|(n, _)| n.clone());
            let Some(oldest) = oldest else { break };
            self.entries.remove(&oldest);
            dropped.push(oldest);
        }
        Ok(dropped)
    }

    /// The bytes to draw, if the host half has sent them yet.
    pub fn get(&self, name: &LiveName) -> Option<Vec<u8>> {
        let entry = self.entries.get(name)?;
        entry.drawn.set(self.tick());
        Some(entry.bytes.clone())
    }

    fn tick(&self) -> u64 {
        self.clock.set(self.clock.get() + 1);
        self.clock.get()
    }

    fn size(&self) -> usize {
        self.entries.values().map(|e| e.bytes.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(n: &str) -> LiveName {
        LiveName::parse(n).unwrap()
    }

    #[test]
    fn stores_replaces_and_removes() {
        let mut cache = LiveCache::default();
        cache.put(name("cover"), vec![1]).unwrap();
        cache.put(name("cover"), vec![2]).unwrap();
        assert_eq!(cache.get(&name("cover")), Some(vec![2]));
        cache.put(name("cover"), vec![]).unwrap();
        assert_eq!(cache.get(&name("cover")), None);
        assert!(cache.put(name("huge"), vec![0; MAX_LIVE_RESOURCE_BYTES + 1]).is_err());
    }

    #[test]
    fn drops_the_least_recently_drawn_to_stay_in_budget() {
        let mut cache = LiveCache::default();
        let mb = vec![0; 1024 * 1024];
        for n in ["a", "b", "c", "d"] {
            assert!(cache.put(name(n), mb.clone()).unwrap().is_empty());
        }
        cache.get(&name("a"));
        assert_eq!(cache.put(name("e"), mb.clone()).unwrap(), vec![name("b")]);
        assert!(cache.get(&name("a")).is_some());
        assert!(cache.get(&name("b")).is_none());
    }
}
