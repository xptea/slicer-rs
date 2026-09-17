//! Bounded revision-aware decoded-frame cache.
//!
//! Cache entries are keyed by asset, decoder/source instance, project
//! revision, and exact source time.  Seek generation is not part of the key;
//! a hit is retagged for the requesting generation before it is delivered.

use super::api::{FrameCacheKey, FrameLease, WorkTag};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheInsertStatus {
    Inserted,
    RejectedOversize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CacheInsertOutcome {
    pub status: CacheInsertStatus,
    pub evicted_entries: usize,
    pub evicted_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheStats {
    pub entries: usize,
    pub bytes: usize,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}

struct CacheEntry {
    frame: FrameLease,
    last_used: u64,
}

/// An LRU cache with independent entry-count and byte budgets.
pub struct FrameCache {
    max_entries: usize,
    max_bytes: usize,
    bytes: usize,
    tick: u64,
    entries: HashMap<FrameCacheKey, CacheEntry>,
    hits: u64,
    misses: u64,
    evictions: u64,
}

impl FrameCache {
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            max_entries,
            max_bytes,
            bytes: 0,
            tick: 0,
            entries: HashMap::new(),
            hits: 0,
            misses: 0,
            evictions: 0,
        }
    }

    pub fn max_entries(&self) -> usize {
        self.max_entries
    }

    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            entries: self.len(),
            bytes: self.bytes,
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
        }
    }

    /// Insert a frame, evicting least-recently-used entries until both bounds
    /// hold.  A frame larger than the byte budget is never retained.
    pub fn insert(&mut self, frame: FrameLease) -> CacheInsertOutcome {
        let bytes = frame.byte_len();
        if self.max_entries == 0 || self.max_bytes == 0 || bytes > self.max_bytes {
            return CacheInsertOutcome {
                status: CacheInsertStatus::RejectedOversize,
                evicted_entries: 0,
                evicted_bytes: 0,
            };
        }
        let key = frame.key;
        if let Some(previous) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(previous.frame.byte_len());
        }
        self.tick = self.tick.wrapping_add(1);
        self.bytes = self.bytes.saturating_add(bytes);
        self.entries.insert(
            key,
            CacheEntry {
                frame,
                last_used: self.tick,
            },
        );

        let mut evicted_entries: usize = 0;
        let mut evicted_bytes: usize = 0;
        while self.entries.len() > self.max_entries || self.bytes > self.max_bytes {
            let Some((&oldest_key, _)) =
                self.entries.iter().min_by_key(|(_, entry)| entry.last_used)
            else {
                break;
            };
            if let Some(oldest) = self.entries.remove(&oldest_key) {
                let oldest_bytes = oldest.frame.byte_len();
                self.bytes = self.bytes.saturating_sub(oldest_bytes);
                evicted_entries += 1;
                evicted_bytes = evicted_bytes.saturating_add(oldest_bytes);
                self.evictions = self.evictions.saturating_add(1);
            }
        }
        CacheInsertOutcome {
            status: CacheInsertStatus::Inserted,
            evicted_entries,
            evicted_bytes,
        }
    }

    /// Retrieve and retag a cache hit for the requesting seek generation.
    pub fn get(&mut self, key: FrameCacheKey, tag: WorkTag) -> Option<FrameLease> {
        self.tick = self.tick.wrapping_add(1);
        let Some(entry) = self.entries.get_mut(&key) else {
            self.misses = self.misses.saturating_add(1);
            return None;
        };
        entry.last_used = self.tick;
        match entry.frame.retag(tag) {
            Ok(frame) => {
                self.hits = self.hits.saturating_add(1);
                Some(frame)
            }
            Err(_) => {
                self.misses = self.misses.saturating_add(1);
                None
            }
        }
    }

    /// Read a frame without changing its generation.  This is useful to an
    /// export worker that has already validated the frame's tag.
    pub fn get_raw(&mut self, key: FrameCacheKey) -> Option<FrameLease> {
        self.tick = self.tick.wrapping_add(1);
        let Some(entry) = self.entries.get_mut(&key) else {
            self.misses = self.misses.saturating_add(1);
            return None;
        };
        entry.last_used = self.tick;
        self.hits = self.hits.saturating_add(1);
        Some(entry.frame.clone())
    }

    pub fn contains(&self, key: FrameCacheKey) -> bool {
        self.entries.contains_key(&key)
    }

    pub fn remove(&mut self, key: FrameCacheKey) -> Option<FrameLease> {
        let entry = self.entries.remove(&key)?;
        self.bytes = self.bytes.saturating_sub(entry.frame.byte_len());
        Some(entry.frame)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    /// Drop entries from a different committed revision.  This is optional
    /// housekeeping; a project-id/revision cache key already prevents reuse.
    pub fn retain_revision(&mut self, revision: u64) {
        let keys = self
            .entries
            .keys()
            .filter(|key| key.revision != revision)
            .copied()
            .collect::<Vec<_>>();
        for key in keys {
            let _ = self.remove(key);
        }
    }
}
