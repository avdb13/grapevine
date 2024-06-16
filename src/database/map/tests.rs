use std::{
    borrow::Borrow,
    collections::BTreeMap,
    marker::PhantomData,
    sync::{RwLock, RwLockReadGuard},
};

use frunk::{hlist, HList};
use futures_util::{stream, Stream, StreamExt};

use super::{FromBytes, Map, MapError, ToBytes};

mod conversions;

struct AliasableBox<T>(*mut T);
impl<T> Drop for AliasableBox<T> {
    fn drop(&mut self) {
        // SAFETY: This is cursed and relies on non-local reasoning.
        //
        // In order for this to be safe:
        //
        // * All aliased references to this value must have been dropped first,
        //   for example by coming after its referrers in struct fields, because
        //   struct fields are automatically dropped in order from top to bottom
        //   in the absence of an explicit Drop impl. Otherwise, the referrers
        //   may read into deallocated memory.
        // * This type must not be copyable or cloneable. Otherwise, double-free
        //   can occur.
        //
        // These conditions are met, but again, note that changing safe code in
        // this module can result in unsoundness if any of these constraints are
        // violated.
        unsafe { drop(Box::from_raw(self.0)) }
    }
}

struct Iter<'a, T, I> {
    inner: I,

    // Needs to outlive `inner` for memory safety reasons
    #[allow(dead_code)]
    guard_ref: AliasableBox<RwLockReadGuard<'a, T>>,
}

impl<T, I> Iterator for Iter<'_, T, I>
where
    I: Iterator,
{
    type Item = I::Item;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next()
    }
}

struct TestMap<K, V> {
    storage: RwLock<BTreeMap<Vec<u8>, Vec<u8>>>,
    types: PhantomData<(K, V)>,
}

impl<K, V> TestMap<K, V> {
    fn new() -> Self {
        Self {
            storage: RwLock::new(BTreeMap::new()),
            types: PhantomData,
        }
    }
}

impl<K, V> Map for TestMap<K, V>
where
    K: ToBytes + FromBytes,
    V: ToBytes + FromBytes,
{
    type Key = K;
    type Value = V;

    async fn get<KB>(&self, key: &KB) -> Result<Option<Self::Value>, MapError>
    where
        Self::Key: Borrow<KB>,
        KB: ToBytes + ?Sized,
    {
        self.storage
            .read()
            .expect("lock should not be poisoned")
            .get(key.borrow().to_bytes().as_ref())
            .map(|v| {
                Self::Value::from_bytes(v.to_owned())
                    .map_err(MapError::FromBytes)
            })
            .transpose()
    }

    async fn set<KB, VB>(&self, key: &KB, value: &VB) -> Result<(), MapError>
    where
        Self::Key: Borrow<KB>,
        Self::Value: Borrow<VB>,
        KB: ToBytes + ?Sized,
        VB: ToBytes + ?Sized,
    {
        self.storage.write().expect("lock should not be poisoned").insert(
            key.borrow().to_bytes().into_owned(),
            value.borrow().to_bytes().into_owned(),
        );

        Ok(())
    }

    async fn del<KB>(&self, key: &KB) -> Result<(), MapError>
    where
        Self::Key: Borrow<KB>,
        KB: ToBytes + ?Sized,
    {
        self.storage
            .write()
            .expect("lock should not be poisoned")
            .remove(key.borrow().to_bytes().as_ref());

        Ok(())
    }

    #[rustfmt::skip]
    async fn scan_prefix<P>(
        &self,
        key: &P,
    ) -> Result<
        impl Stream<
            Item = (Result<Self::Key, MapError>, Result<Self::Value, MapError>)
        >,
        MapError,
    >
    where
        P: ToBytes,
    {
        let guard = self
            .storage
            .read()
            .expect("lock should not be poisoned");

        let guard = Box::leak(Box::new(guard));

        let guard_ref = AliasableBox(guard);

        let inner = guard
            .iter()
            .filter(|(kb, _)| kb.starts_with(key.borrow().to_bytes().as_ref()))
            .map(|(kb, vb)| {
                (
                    Self::Key::from_bytes(kb.to_owned())
                        .map_err(MapError::FromBytes),
                    Self::Value::from_bytes(vb.to_owned())
                        .map_err(MapError::FromBytes),
                )
            });

        Ok(stream::iter(Iter {
            inner,
            guard_ref,
        }))
    }
}

#[tokio::test]
async fn string_to_string() {
    let test_map = TestMap::<String, String>::new();

    let key = "hello".to_owned();
    let value = "world".to_owned();

    test_map.set(&key, &value).await.expect("insertion should succed");

    let actual_value = test_map.get(&key).await.expect("lookup should succeed");

    assert_eq!(Some(value), actual_value);

    test_map.del(&key).await.expect("deletion should succeed");

    let actual_value = test_map.get(&key).await.expect("lookup should succeed");

    assert_eq!(None, actual_value);
}

#[tokio::test]
async fn hlist_to_hlist() {
    let test_map =
        TestMap::<HList![String, String], HList![String, String]>::new();

    let key = hlist!["hello".to_owned(), "world".to_owned()];
    let value = hlist!["test".to_owned(), "suite".to_owned()];

    test_map.set(&key, &value).await.expect("insertion should succed");

    let actual_value = test_map.get(&key).await.expect("lookup should succeed");

    assert_eq!(Some(value), actual_value);

    test_map.del(&key).await.expect("deletion should succeed");

    let actual_value = test_map.get(&key).await.expect("lookup should succeed");

    assert_eq!(None, actual_value);
}

#[tokio::test]
async fn hlist_scan_prefix() {
    let test_map =
        TestMap::<HList![String, String], HList![String, String]>::new();

    let key = hlist!["hello".to_owned(), "world".to_owned()];
    let value = hlist!["test".to_owned(), "suite".to_owned()];
    test_map.set(&key, &value).await.expect("insertion should succed");

    let key = hlist!["hello".to_owned(), "debugger".to_owned()];
    let value = hlist!["tester".to_owned(), "suiter".to_owned()];
    test_map.set(&key, &value).await.expect("insertion should succed");

    let key = hlist!["shouldn't".to_owned(), "appear".to_owned()];
    let value = hlist!["in".to_owned(), "assertions".to_owned()];
    test_map.set(&key, &value).await.expect("insertion should succed");

    let prefix = hlist!["hello".to_owned()];
    let mut stream = test_map
        .scan_prefix(&prefix)
        .await
        .expect("scanning should succeed")
        .enumerate();
    while let Some((i, next)) = stream.next().await {
        let (key, value) = next;
        let (key, value) = (
            key.expect("key decoding should succeed"),
            value.expect("value decoding should succeed"),
        );

        // Ordering is guaranteed because BTreeMap
        match i {
            0 => {
                assert_eq!(
                    key,
                    hlist!["hello".to_owned(), "debugger".to_owned()]
                );
                assert_eq!(
                    value,
                    hlist!["tester".to_owned(), "suiter".to_owned()]
                );
            }
            1 => {
                assert_eq!(key, hlist!["hello".to_owned(), "world".to_owned()]);
                assert_eq!(
                    value,
                    hlist!["test".to_owned(), "suite".to_owned()]
                );
            }
            _ => unreachable!(),
        }
    }
}
