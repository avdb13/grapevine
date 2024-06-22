use std::{
    collections::HashMap,
    fmt,
    hash::Hash,
    ops::Deref,
    sync::{Arc, Weak},
};

use strum::{AsRefStr, IntoStaticStr};
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
    /// Values are owned by their [entries][Entry]
    entries: RwLock<HashMap<K, Weak<V>>>,
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

        let Some(weak) = map.get(&key) else {
            trace!("entry has already been cleaned up");
            return;
        };

        if weak.strong_count() != 0 {
            trace!("entry is in use");
            return;
        }

        trace!("cleaning up unused entry");
        map.remove(&key);
        METRICS.record_on_demand_hashmap_size(self.name.clone(), map.len());
    }

    #[tracing::instrument(level = Level::TRACE, skip(map))]
    fn try_get_live_value(
        pass: usize,
        map: &HashMap<K, Weak<V>>,
        key: &K,
    ) -> Option<Arc<V>> {
        if let Some(value) = map.get(key) {
            if let Some(value) = value.upgrade() {
                // TODO: increment value's clone counter
                trace!(pass, "using existing value");
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
    async fn get_or_insert_with<F>(
        &self,
        key: &K,
        create: F,
    ) -> (Arc<V>, GetAction)
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
                return (v, GetAction::CloneExisting);
            }
        }

        // no entry or it has died, create a new one
        let value = Arc::new(create());
        let weak = Arc::downgrade(&value);

        {
            // take a write lock, try again, otherwise insert our new value
            let mut map = self.entries.write().await;
            if let Some(v) = Self::try_get_live_value(2, &map, key) {
                // another entry showed up while we had let go of the lock,
                // use that
                drop(value);
                drop(weak);
                return (v, GetAction::CloneExistingAfterCreation);
            }

            map.insert(key.clone(), weak);
            METRICS.record_on_demand_hashmap_size(self.name.clone(), map.len());

            (value, GetAction::InsertNew)
        }
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
        let (value, get_action) =
            self.shared.get_or_insert_with(&key, create).await;
        METRICS
            .record_on_demand_hashmap_get(self.shared.name.clone(), get_action);

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

/// Action performed during [`OnDemandHashMap::get_or_insert_with()`], for
/// metrics.
#[derive(Clone, Copy, AsRefStr, IntoStaticStr)]
pub(crate) enum GetAction {
    /// An existing entry was cheaply cloned.
    CloneExisting,
    /// A new entry was created, but the creation was raced by another task and
    /// the created entry was dropped.
    CloneExistingAfterCreation,
    /// A new entry was created and inserted into the map.
    InsertNew,
}
