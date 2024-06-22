use std::{
    collections::HashMap,
    fmt,
    hash::Hash,
    ops::Deref,
    sync::{
        atomic::{self, AtomicUsize},
        Arc, Weak,
    },
};

use tokio::sync::{mpsc, RwLock};
use tracing::{trace, warn, Level};

use crate::observability::METRICS;

/// Data shared between [`OnDemandHashMap`] and the cleanup task
///
/// Importantly it does not contain the `cleanup_sender`, since it getting
/// dropped signals the cleanup task to exit. If the cleanup task had an owned
/// reference to it, the only way for it to exit would be for every [`Entry`] to
/// be dropped, we don't want to rely on that.
struct SharedData<K, V> {
    name: Arc<str>,
    /// The inner backing storage.
    ///
    /// Each entry consists of a clone count and the value itself, which is
    /// owned by its [entries][Entry].
    entries: RwLock<HashMap<K, (AtomicUsize, Weak<V>)>>,
}

impl<K, V> SharedData<K, V>
where
    K: Hash + Eq + Clone + fmt::Debug,
{
    #[tracing::instrument(
        level = Level::TRACE,
        skip(self),
        fields(name = self.name.as_ref()),
    )]
    async fn try_cleanup_entry(&self, key: K) {
        let mut map = self.entries.write().await;

        let Some((clone_count, weak)) = map.get(&key) else {
            trace!("entry has already been cleaned up");
            return;
        };

        if weak.strong_count() != 0 {
            trace!("entry is in use");
            return;
        }

        trace!("cleaning up unused entry");
        let clone_count = clone_count.load(atomic::Ordering::Relaxed);
        map.remove(&key);
        METRICS.record_on_demand_hashmap_size(self.name.clone(), map.len());
        METRICS.record_on_demand_hashmap_clone_count(
            self.name.clone(),
            clone_count,
        );
    }

    #[tracing::instrument(level = Level::TRACE, skip(map))]
    fn try_get_live_value(
        pass: usize,
        map: &HashMap<K, (AtomicUsize, Weak<V>)>,
        key: &K,
    ) -> Option<Arc<V>> {
        if let Some((clone_count, value)) = map.get(key) {
            if let Some(value) = value.upgrade() {
                trace!(pass, "using existing value");
                clone_count.fetch_add(1, atomic::Ordering::Relaxed);
                return Some(value);
            }

            trace!(
                pass,
                "existing value is stale and needs cleanup, creating new"
            );
        } else {
            trace!(pass, "no existing value, creating new");
        }

        None
    }

    /// Either returns an existing live value, or creates a new one and inserts
    /// it into the map.
    #[tracing::instrument(level = Level::TRACE, skip(self, create))]
    async fn get_or_insert_with<F>(&self, key: &K, create: F) -> Arc<V>
    where
        F: FnOnce() -> V,
    {
        {
            // first, take a read lock and try to get an existing value

            // TODO check if this fast path actually makes it faster, possibly
            // make it configurable per OnDemandHashMap depending on contention
            // and how expensive create() is
            let map = self.entries.read().await;
            if let Some(v) = Self::try_get_live_value(1, &map, key) {
                return v;
            }
        }

        // no entry or it has died, create a new one
        let value = Arc::new(create());
        let weak = Arc::downgrade(&value);

        // take a write lock, try again, otherwise insert our new value
        let mut map = self.entries.write().await;
        if let Some(v) = Self::try_get_live_value(2, &map, key) {
            // another entry showed up while we had let go of the lock,
            // use that
            drop(value);
            drop(weak);
            return v;
        }

        map.insert(key.clone(), (AtomicUsize::new(0), weak));
        METRICS.record_on_demand_hashmap_size(self.name.clone(), map.len());

        value
    }
}

/// A [`HashMap`] whose entries are automatically removed once they are no
/// longer referenced.
pub(crate) struct OnDemandHashMap<K, V> {
    /// The data shared between the [`OnDemandHashMap`] and the cleanup task.
    shared: Arc<SharedData<K, V>>,
    /// This is the only non-[weak][mpsc::WeakUnboundedSender] `Sender`, which
    /// means that dropping the `OnDemandHashMap` causes the cleanup
    /// process to exit.
    cleanup_sender: mpsc::UnboundedSender<K>,
}

impl<K, V> OnDemandHashMap<K, V>
where
    K: Hash + Eq + Clone + fmt::Debug + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    /// Creates a new `OnDemandHashMap`. The `name` is used for metrics and
    /// should be unique to this instance.
    pub(crate) fn new(name: String) -> Self {
        let (cleanup_sender, mut receiver) = mpsc::unbounded_channel();

        let shared = Arc::new(SharedData {
            name: name.into(),
            entries: RwLock::new(HashMap::new()),
        });

        {
            let shared = Arc::clone(&shared);
            tokio::task::spawn(async move {
                loop {
                    let Some(key) = receiver.recv().await else {
                        trace!(
                            name = shared.name.as_ref(),
                            "channel has died, exiting cleanup task"
                        );
                        return;
                    };

                    shared.try_cleanup_entry(key).await;
                }
            });
        }

        Self {
            shared,
            cleanup_sender,
        }
    }

    #[tracing::instrument(level = Level::TRACE, skip(self, create))]
    pub(crate) async fn get_or_insert_with<F>(
        &self,
        key: K,
        create: F,
    ) -> Entry<K, V>
    where
        F: FnOnce() -> V,
    {
        let value = self.shared.get_or_insert_with(&key, create).await;

        Entry {
            cleanup_sender: self.cleanup_sender.downgrade(),
            key: Some(key),
            value,
        }
    }
}

/// A wrapper around a key/value pair inside an [`OnDemandHashMap`]
///
/// If every `Entry` for a specific key is dropped, the value is removed from
/// the map.
pub(crate) struct Entry<K, V> {
    cleanup_sender: mpsc::WeakUnboundedSender<K>,
    /// Only `None` during `drop()`
    key: Option<K>,
    value: Arc<V>,
}

impl<K, V> Deref for Entry<K, V> {
    type Target = V;

    fn deref(&self) -> &Self::Target {
        self.value.as_ref()
    }
}

impl<K, V> Drop for Entry<K, V> {
    fn drop(&mut self) {
        let Some(cleanup_sender) = self.cleanup_sender.upgrade() else {
            trace!("backing map has already been dropped");
            return;
        };

        if let Err(error) = cleanup_sender
            .send(self.key.take().expect("drop should only be called once"))
        {
            warn!(%error, "Failed to send cleanup message");
        };
    }
}
