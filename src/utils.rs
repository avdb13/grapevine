pub(crate) mod error;

use std::{
    borrow::Cow,
    cmp, fmt,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use argon2::{Config, Variant};
use cmp::Ordering;
use rand::prelude::*;
use ring::digest;
use ruma::{
    canonical_json::try_from_json_map, CanonicalJsonError, CanonicalJsonObject,
};

// Hopefully we have a better chat protocol in 530 years
#[allow(clippy::as_conversions, clippy::cast_possible_truncation)]
pub(crate) fn millis_since_unix_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time is valid")
        .as_millis() as u64
}

#[cfg(any(feature = "rocksdb", feature = "sqlite"))]
pub(crate) fn increment(old: Option<&[u8]>) -> Vec<u8> {
    let number = match old.map(TryInto::try_into) {
        Some(Ok(bytes)) => {
            let number = u64::from_be_bytes(bytes);
            number + 1
        }
        // Start at one. since 0 should return the first event in the db
        _ => 1,
    };

    number.to_be_bytes().to_vec()
}

pub(crate) fn generate_keypair() -> Vec<u8> {
    let mut value = random_string(8).as_bytes().to_vec();
    value.push(0xFF);
    value.extend_from_slice(
        &ruma::signatures::Ed25519KeyPair::generate()
            .expect("Ed25519KeyPair generation always works (?)"),
    );
    value
}

/// Parses the bytes into an u64.
pub(crate) fn u64_from_bytes(
    bytes: &[u8],
) -> Result<u64, std::array::TryFromSliceError> {
    let array: [u8; 8] = bytes.try_into()?;
    Ok(u64::from_be_bytes(array))
}

/// Parses the bytes into a string.
pub(crate) fn string_from_bytes(
    bytes: &[u8],
) -> Result<String, std::string::FromUtf8Error> {
    String::from_utf8(bytes.to_vec())
}

pub(crate) fn random_string(length: usize) -> String {
    thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(length)
        .map(char::from)
        .collect()
}

/// Calculate a new hash for the given password
pub(crate) fn calculate_password_hash(
    password: &str,
) -> Result<String, argon2::Error> {
    let hashing_config = Config {
        variant: Variant::Argon2id,
        ..Default::default()
    };

    let salt = random_string(32);
    argon2::hash_encoded(password.as_bytes(), salt.as_bytes(), &hashing_config)
}

#[tracing::instrument(skip(keys))]
pub(crate) fn calculate_hash(keys: &[&[u8]]) -> Vec<u8> {
    // We only hash the pdu's event ids, not the whole pdu
    let bytes = keys.join(&0xFF);
    let hash = digest::digest(&digest::SHA256, &bytes);
    hash.as_ref().to_owned()
}

pub(crate) fn common_elements<I, F>(
    mut iterators: I,
    check_order: F,
) -> Option<impl Iterator<Item = Vec<u8>>>
where
    I: Iterator,
    I::Item: Iterator<Item = Vec<u8>>,
    F: Fn(&[u8], &[u8]) -> Ordering,
{
    let first_iterator = iterators.next()?;
    let mut other_iterators =
        iterators.map(Iterator::peekable).collect::<Vec<_>>();

    Some(first_iterator.filter(move |target| {
        other_iterators.iter_mut().all(|it| {
            while let Some(element) = it.peek() {
                match check_order(element, target) {
                    // We went too far
                    Ordering::Greater => return false,
                    // Element is in both iters
                    Ordering::Equal => return true,
                    // Keep searching
                    Ordering::Less => {
                        it.next();
                    }
                }
            }
            false
        })
    }))
}

/// Fallible conversion from any value that implements `Serialize` to a
/// `CanonicalJsonObject`.
///
/// `value` must serialize to an `serde_json::Value::Object`.
pub(crate) fn to_canonical_object<T: serde::Serialize>(
    value: T,
) -> Result<CanonicalJsonObject, CanonicalJsonError> {
    use serde::ser::Error;

    match serde_json::to_value(value).map_err(CanonicalJsonError::SerDe)? {
        serde_json::Value::Object(map) => try_from_json_map(map),
        _ => Err(CanonicalJsonError::SerDe(serde_json::Error::custom(
            "Value must be an object",
        ))),
    }
}

pub(crate) fn deserialize_from_str<
    'de,
    D: serde::de::Deserializer<'de>,
    T: FromStr<Err = E>,
    E: std::fmt::Display,
>(
    deserializer: D,
) -> Result<T, D::Error> {
    struct Visitor<T: FromStr<Err = E>, E>(std::marker::PhantomData<T>);
    impl<T: FromStr<Err = Err>, Err: std::fmt::Display> serde::de::Visitor<'_>
        for Visitor<T, Err>
    {
        type Value = T;

        fn expecting(
            &self,
            formatter: &mut std::fmt::Formatter<'_>,
        ) -> std::fmt::Result {
            write!(formatter, "a parsable string")
        }

        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            v.parse().map_err(serde::de::Error::custom)
        }
    }
    deserializer.deserialize_str(Visitor(std::marker::PhantomData))
}

/// Debug-formats the given slice, but only up to the first `max_len` elements.
/// Any further elements are replaced by an ellipsis.
///
/// See also [`debug_slice_truncated()`],
pub(crate) struct TruncatedDebugSlice<'a, T> {
    inner: &'a [T],
    max_len: usize,
}

impl<T: fmt::Debug> fmt::Debug for TruncatedDebugSlice<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.inner.len() <= self.max_len {
            write!(f, "{:?}", self.inner)
        } else {
            f.debug_list()
                .entries(&self.inner[..self.max_len])
                .entry(&"...")
                .finish()
        }
    }
}

/// See [`TruncatedDebugSlice`]. Useful for `#[instrument]`:
///
/// ```ignore
/// #[tracing::instrument(fields(
///     foos = debug_slice_truncated(foos, N)
/// ))]
/// ```
pub(crate) fn debug_slice_truncated<T: fmt::Debug>(
    slice: &[T],
    max_len: usize,
) -> tracing::field::DebugValue<TruncatedDebugSlice<'_, T>> {
    tracing::field::debug(TruncatedDebugSlice {
        inner: slice,
        max_len,
    })
}

/// Truncates a string to an approximate maximum length, replacing any extra
/// text with an ellipsis.
///
/// Only to be used for debug logging, exact semantics are unspecified.
pub(crate) fn truncate_str_for_debug(
    s: &str,
    mut max_len: usize,
) -> Cow<'_, str> {
    while max_len < s.len() && !s.is_char_boundary(max_len) {
        max_len += 1;
    }

    if s.len() <= max_len {
        s.into()
    } else {
        #[allow(clippy::string_slice)] // we checked it's at a char boundary
        format!("{}...", &s[..max_len]).into()
    }
}

#[cfg(test)]
mod tests {
    use crate::utils::truncate_str_for_debug;

    #[test]
    fn test_truncate_str_for_debug() {
        assert_eq!(truncate_str_for_debug("short", 10), "short");
        assert_eq!(
            truncate_str_for_debug("very long string", 10),
            "very long ..."
        );
        assert_eq!(truncate_str_for_debug("no info, only dots", 0), "...");
        assert_eq!(truncate_str_for_debug("", 0), "");
        assert_eq!(truncate_str_for_debug("unicöde", 5), "unicö...");
        let ok_hand = "👌🏽";
        assert_eq!(truncate_str_for_debug(ok_hand, 1), "👌...");
        assert_eq!(truncate_str_for_debug(ok_hand, ok_hand.len() - 1), "👌🏽");
        assert_eq!(truncate_str_for_debug(ok_hand, ok_hand.len()), "👌🏽");
    }
}
