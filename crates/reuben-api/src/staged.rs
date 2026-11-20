//! A store whose writes wait in memory until committed.
//!
//! An edit is applied, the whole instrument is checked to still load and plan, and only then does
//! [`Staged::commit`] write to the inner store. Until then every read through the wrapper sees the
//! pending text and the inner store sees nothing.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use std::sync::{Mutex, MutexGuard};

use crate::resources::{Created, ResolveError, Resources, SampleBuffer};

struct Shared<S> {
    inner: S,
    /// Keyed by `inner.canonical(source, None)`, so two spellings of one source are one write.
    pending: Mutex<BTreeMap<String, String>>,
    /// `Some` while a [`ReadLog`] is alive.
    reads: Arc<ReadSlot>,
}

type ReadSlot = Mutex<Option<BTreeSet<String>>>;

/// A [`Resources`] store that holds [`write_text`](Resources::write_text) in memory until
/// [`commit`](Self::commit). Clones share one pending set.
///
/// Neither lock is held across a call into the inner store.
pub struct Staged<S> {
    shared: Arc<Shared<S>>,
}

impl<S> Clone for Staged<S> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

/// Recording of every canonical source [`Staged::read_text`](Resources::read_text) served since
/// [`Staged::record_reads`]. Dropping it without [`finish`](Self::finish) stops the recording.
pub struct ReadLog {
    reads: Arc<ReadSlot>,
}

impl ReadLog {
    /// Stop recording and return the canonical sources read.
    pub fn finish(self) -> BTreeSet<String> {
        lock(&self.reads).take().unwrap_or_default()
    }
}

impl Drop for ReadLog {
    fn drop(&mut self) {
        lock(&self.reads).take();
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl<S: Resources> Staged<S> {
    pub fn new(inner: S) -> Self {
        Self {
            shared: Arc::new(Shared {
                inner,
                pending: Mutex::new(BTreeMap::new()),
                reads: Arc::new(Mutex::new(None)),
            }),
        }
    }

    /// Canonical sources with a pending write, sorted.
    pub fn pending(&self) -> Vec<String> {
        lock(&self.shared.pending).keys().cloned().collect()
    }

    /// Write every pending text to the inner store in sorted source order and clear pending. Stops
    /// at the first failure and returns it; pending is cleared either way.
    pub fn commit(&self) -> Result<(), ResolveError> {
        let drained = core::mem::take(&mut *lock(&self.shared.pending));
        for (source, text) in &drained {
            self.shared.inner.write_text(source, text)?;
        }
        Ok(())
    }

    /// Drop every pending write.
    pub fn discard(&self) {
        lock(&self.shared.pending).clear();
    }

    /// Start recording the canonical source of every text read served, to learn which documents
    /// a load reached.
    pub fn record_reads(&self) -> ReadLog {
        *lock(&self.shared.reads) = Some(BTreeSet::new());
        ReadLog {
            reads: Arc::clone(&self.shared.reads),
        }
    }

    fn key(&self, source: &str) -> String {
        self.shared.inner.canonical(source, None)
    }
}

impl<S: Resources> Resources for Staged<S> {
    fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        self.shared.inner.read_samples(source)
    }

    fn read_text(&self, source: &str) -> Result<String, ResolveError> {
        let key = self.key(source);
        let pending = lock(&self.shared.pending).get(&key).cloned();
        let text = match pending {
            Some(text) => text,
            None => self.shared.inner.read_text(source)?,
        };
        if let Some(log) = lock(&self.shared.reads).as_mut() {
            log.insert(key);
        }
        Ok(text)
    }

    fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
        let key = self.key(source);
        lock(&self.shared.pending).insert(key, text.to_string());
        Ok(())
    }

    fn create_text(&self, source: &str, text: &str) -> Result<Created, ResolveError> {
        let key = self.key(source);
        let pending = lock(&self.shared.pending).contains_key(&key);
        if pending {
            return Ok(Created::Taken);
        }
        match self.shared.inner.read_text(source) {
            Ok(_) => return Ok(Created::Taken),
            Err(ResolveError::NotFound(_)) => {}
            Err(other) => return Err(other),
        }
        self.write_text(source, text).map(|()| Created::Written)
    }

    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        self.shared.inner.canonical(source, referrer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;

    #[derive(Default)]
    struct Map(Mutex<BTreeMap<String, String>>);

    impl Map {
        fn with(pairs: &[(&str, &str)]) -> Self {
            Self(Mutex::new(
                pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ))
        }
    }

    impl Resources for Map {
        fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
            Err(ResolveError::NotFound(source.to_string()))
        }
        fn read_text(&self, source: &str) -> Result<String, ResolveError> {
            lock(&self.0)
                .get(source)
                .cloned()
                .ok_or_else(|| ResolveError::NotFound(source.to_string()))
        }
        fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
            lock(&self.0).insert(source.to_string(), text.to_string());
            Ok(())
        }
    }

    #[test]
    fn write_is_invisible_to_inner_until_commit() {
        let inner = Arc::new(Map::with(&[("a", "old")]));
        let staged = Staged::new(Arc::clone(&inner));
        staged.write_text("a", "new").unwrap();
        assert_eq!(staged.read_text("a").unwrap(), "new");
        assert_eq!(inner.read_text("a").unwrap(), "old");
        assert_eq!(staged.pending(), ["a"]);
        staged.commit().unwrap();
        assert_eq!(inner.read_text("a").unwrap(), "new");
        assert!(staged.pending().is_empty());
    }

    #[test]
    fn discard_drops_pending() {
        let staged = Staged::new(Map::with(&[("a", "old")]));
        staged.write_text("a", "new").unwrap();
        staged.discard();
        assert_eq!(staged.read_text("a").unwrap(), "old");
    }

    #[test]
    fn clone_shares_pending() {
        let one = Staged::new(Map::with(&[("a", "old")]));
        let two = one.clone();
        one.write_text("a", "new").unwrap();
        assert_eq!(two.read_text("a").unwrap(), "new");
    }

    #[test]
    fn read_log_records_canonical_sources() {
        let staged = Staged::new(Map::with(&[("a", "1"), ("b", "2")]));
        let log = staged.record_reads();
        staged.read_text("a").unwrap();
        staged.read_text("b").unwrap();
        let seen = log.finish();
        assert_eq!(seen, BTreeSet::from(["a".to_string(), "b".to_string()]));
        staged.read_text("a").unwrap();
        assert!(lock(&staged.shared.reads).is_none());
    }

    #[test]
    fn create_sees_pending() {
        let staged = Staged::new(Map::default());
        staged.write_text("a", "x").unwrap();
        assert_eq!(staged.create_text("a", "y").unwrap(), Created::Taken);
        assert_eq!(staged.create_text("b", "y").unwrap(), Created::Written);
        assert_eq!(staged.read_text("b").unwrap(), "y");
    }

    /// Fails every write to `bad`, and runs `on_read` inside every read.
    #[derive(Default)]
    struct Probe {
        map: Map,
        bad: &'static str,
        unreadable: &'static str,
        on_read: Mutex<Option<alloc::boxed::Box<dyn Fn() + Send>>>,
    }

    impl Resources for Probe {
        fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
            self.map.read_samples(source)
        }
        fn read_text(&self, source: &str) -> Result<String, ResolveError> {
            if let Some(f) = lock(&self.on_read).as_ref() {
                f();
            }
            if source == self.unreadable {
                return Err(ResolveError::Decode(source.to_string()));
            }
            self.map.read_text(source)
        }
        fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
            if source == self.bad {
                return Err(ResolveError::Write(source.to_string()));
            }
            self.map.write_text(source, text)
        }
    }

    #[test]
    fn create_does_not_hold_pending_across_inner_read() {
        let inner = Arc::new(Probe::default());
        let staged = Staged::new(Arc::clone(&inner));
        let reentrant = staged.clone();
        *lock(&inner.on_read) = Some(alloc::boxed::Box::new(move || {
            reentrant.pending();
        }));
        assert_eq!(staged.create_text("a", "x").unwrap(), Created::Written);
        *lock(&inner.on_read) = None;
    }

    #[test]
    fn create_over_an_unreadable_source_is_an_error_not_free() {
        let staged = Staged::new(Probe {
            unreadable: "a",
            ..Probe::default()
        });
        assert!(matches!(
            staged.create_text("a", "x"),
            Err(ResolveError::Decode(_))
        ));
        assert!(staged.pending().is_empty());
    }

    #[test]
    fn commit_stops_at_first_failure_and_clears_pending() {
        let inner = Arc::new(Probe {
            bad: "b",
            ..Probe::default()
        });
        let staged = Staged::new(Arc::clone(&inner));
        for s in ["a", "b", "c"] {
            staged.write_text(s, "new").unwrap();
        }
        assert!(matches!(staged.commit(), Err(ResolveError::Write(_))));
        assert!(staged.pending().is_empty());
        assert_eq!(inner.map.read_text("a").unwrap(), "new");
        assert!(inner.map.read_text("c").is_err());
    }

    #[test]
    fn boxed_dyn_store_is_send_sync_and_a_store() {
        fn needs<T: Resources + Send + Sync + Clone + 'static>() {}
        needs::<Staged<alloc::boxed::Box<dyn Resources + Send + Sync>>>();
    }
}
