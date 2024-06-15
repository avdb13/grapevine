use std::{
    borrow::Borrow, collections::BTreeMap, marker::PhantomData, sync::RwLock,
};

use frunk::{hlist, HList};

use super::{FromBytes, Map, MapError, ToBytes};

mod conversions;

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
