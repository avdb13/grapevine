//! A high-level strongly-typed abstraction over key-value stores

#![warn(missing_docs, clippy::missing_docs_in_private_items)]

use std::{
    any::TypeId,
    borrow::{Borrow, Cow},
    error::Error,
};

use frunk::{HCons, HNil};
use futures_util::Stream;

#[cfg(test)]
mod tests;

/// Errors that can occur during key-value store operations
// Missing docs are allowed here since that kind of information should be
// encoded in the error messages themselves anyway.
#[allow(clippy::missing_docs_in_private_items, dead_code)]
#[derive(thiserror::Error, Debug)]
pub(crate) enum MapError {
    #[cfg(feature = "sqlite")]
    #[error("sqlite error")]
    Sqlite(#[from] rusqlite::Error),

    #[cfg(feature = "rocksdb")]
    #[error("rocksdb error")]
    Rocksdb(#[from] rocksdb::Error),

    #[error("failed to convert stored value into structured data")]
    FromBytes(#[source] Box<dyn Error>),
}

/// A high-level representation of a key-value relation in a key-value store
#[allow(dead_code)]
pub(crate) trait Map {
    /// The key type of this relation
    type Key: ToBytes + FromBytes;

    /// The value type of this relation
    type Value: ToBytes + FromBytes;

    /// Load a value based on its corresponding key
    async fn get<K>(&self, key: &K) -> Result<Option<Self::Value>, MapError>
    where
        Self::Key: Borrow<K>,
        K: ToBytes + ?Sized;

    /// Insert or update a key-value pair
    async fn set<K, V>(&self, key: &K, value: &V) -> Result<(), MapError>
    where
        Self::Key: Borrow<K>,
        Self::Value: Borrow<V>,
        K: ToBytes + ?Sized,
        V: ToBytes + ?Sized;

    /// Remove a key-value pair by its key
    ///
    /// It is not an error to remove a key-value pair that is not present in the
    /// store.
    async fn del<K>(&self, key: &K) -> Result<(), MapError>
    where
        Self::Key: Borrow<K>,
        K: ToBytes + ?Sized;

    /// Get a stream of all key-value pairs whose key matches a key prefix
    ///
    /// While it's possible to provide an entire key as the prefix, it's likely
    /// more ergonomic and more performant to use [`Map::get`] in that case
    /// instead.
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
        P: ToBytes + IsPrefixOf<Self::Key>;
}

/// Convert `Self` into bytes for storage in a key-value store
///
/// Implementations on types other than `HList`s must not contain `0xFF` bytes
/// in their serialized form.
///
/// [`FromBytes`] must be the exact inverse of this operation.
#[allow(dead_code)]
pub(crate) trait ToBytes {
    /// Perform the conversion
    fn to_bytes(&self) -> Cow<'_, [u8]>;
}

impl ToBytes for () {
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        Cow::Borrowed(&[])
    }
}

impl ToBytes for HNil {
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        Cow::Borrowed(&[])
    }
}

impl<H, T> ToBytes for HCons<H, T>
where
    H: ToBytes,
    T: ToBytes + 'static,
{
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        let buf = self.head.to_bytes();

        if TypeId::of::<T>() == TypeId::of::<HNil>() {
            buf
        } else {
            let mut buf = buf.into_owned();
            buf.push(0xFF);
            buf.extend_from_slice(self.tail.to_bytes().as_ref());
            Cow::Owned(buf)
        }
    }
}

impl ToBytes for String {
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        Cow::Borrowed(self.as_bytes())
    }
}

/// Convert from bytes stored in a key-value store into structured data
///
/// This should generally only be implemented by owned types.
///
/// [`ToBytes`] must be the exact inverse of this operation.
#[allow(dead_code)]
pub(crate) trait FromBytes
where
    Self: Sized,
{
    /// Perform the conversion
    fn from_bytes(bytes: Vec<u8>) -> Result<Self, Box<dyn Error>>;
}

impl FromBytes for () {
    fn from_bytes(bytes: Vec<u8>) -> Result<Self, Box<dyn Error>> {
        bytes
            .is_empty()
            .then_some(())
            .ok_or_else(|| "got bytes when none were expected".into())
    }
}

impl FromBytes for HNil {
    fn from_bytes(bytes: Vec<u8>) -> Result<Self, Box<dyn Error>> {
        bytes
            .is_empty()
            .then_some(HNil)
            .ok_or_else(|| "got bytes when none were expected".into())
    }
}

impl<H, T> FromBytes for HCons<H, T>
where
    H: FromBytes,
    T: FromBytes + 'static,
{
    fn from_bytes(bytes: Vec<u8>) -> Result<Self, Box<dyn Error>> {
        let (head, tail) = if TypeId::of::<T>() == TypeId::of::<HNil>() {
            // There is no spoon. I mean, tail.
            (bytes, Vec::new())
        } else {
            let boundary = bytes
                .iter()
                .copied()
                .position(|x| x == 0xFF)
                .ok_or("map entry is missing a boundary")?;

            // Don't include the boundary in the head or tail
            let head = &bytes[..boundary];
            let tail = &bytes[boundary + 1..];

            (head.to_owned(), tail.to_owned())
        };

        Ok(HCons {
            head: H::from_bytes(head)?,
            tail: T::from_bytes(tail)?,
        })
    }
}

impl FromBytes for String {
    fn from_bytes(bytes: Vec<u8>) -> Result<Self, Box<dyn Error>> {
        String::from_utf8(bytes).map_err(Into::into)
    }
}

/// Ensures, at compile time, that one `HList` is a prefix of another
pub(crate) trait IsPrefixOf<HList> {}

impl<HList> IsPrefixOf<HList> for HNil {}

impl<Head, PrefixTail, Tail> IsPrefixOf<HCons<Head, Tail>>
    for HCons<Head, PrefixTail>
where
    PrefixTail: IsPrefixOf<Tail>,
{
}
