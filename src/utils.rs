pub(crate) mod error;
pub(crate) mod on_demand_hashmap;

use std::{
    borrow::Cow,
    cmp, fmt,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use argon2::{password_hash, Argon2, PasswordHasher, PasswordVerifier};
use cmp::Ordering;
use rand::{prelude::*, rngs::OsRng};
use ring::digest;
use ruma::{
    api::client::error::ErrorKind, canonical_json::try_from_json_map,
    CanonicalJsonError, CanonicalJsonObject, MxcUri, MxcUriError, OwnedMxcUri,
};

use crate::{Error, Result};

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

/// Hash the given password
pub(crate) fn hash_password<B>(
    password: B,
) -> Result<password_hash::PasswordHashString, password_hash::Error>
where
    B: AsRef<[u8]>,
{
    Argon2::default()
        .hash_password(
            password.as_ref(),
            &password_hash::SaltString::generate(&mut OsRng),
        )
        .map(|x| x.serialize())
}

/// Compare a password to a hash
///
/// Returns `true` if the password matches the hash, `false` otherwise.
pub(crate) fn verify_password<S, B>(hash: S, password: B) -> bool
where
    S: AsRef<str>,
    B: AsRef<[u8]>,
{
    let Ok(hash) = password_hash::PasswordHash::new(hash.as_ref()) else {
        return false;
    };

    Argon2::default().verify_password(password.as_ref(), &hash).is_ok()
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
    E: fmt::Display,
>(
    deserializer: D,
) -> Result<T, D::Error> {
    struct Visitor<T: FromStr<Err = E>, E>(std::marker::PhantomData<T>);
    impl<T: FromStr<Err = Err>, Err: fmt::Display> serde::de::Visitor<'_>
        for Visitor<T, Err>
    {
        type Value = T;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
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
/// Only to be used for informational purposes, exact semantics are unspecified.
pub(crate) fn dbg_truncate_str(s: &str, mut max_len: usize) -> Cow<'_, str> {
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

/// Data that makes up an `mxc://` URL.
#[derive(Debug, Clone)]
pub(crate) struct MxcData<'a> {
    pub(crate) server_name: &'a ruma::ServerName,
    pub(crate) media_id: &'a str,
}

impl<'a> MxcData<'a> {
    pub(crate) fn new(
        server_name: &'a ruma::ServerName,
        media_id: &'a str,
    ) -> Result<Self> {
        if !media_id.bytes().all(|b| {
            matches!(b,
                b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'-' | b'_'
            )
        }) {
            return Err(Error::BadRequest(
                ErrorKind::InvalidParam,
                "Invalid MXC media id",
            ));
        }

        Ok(Self {
            server_name,
            media_id,
        })
    }
}

impl fmt::Display for MxcData<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "mxc://{}/{}", self.server_name, self.media_id)
    }
}

impl From<MxcData<'_>> for OwnedMxcUri {
    fn from(value: MxcData<'_>) -> Self {
        value.to_string().into()
    }
}

impl<'a> TryFrom<&'a MxcUri> for MxcData<'a> {
    type Error = MxcUriError;

    fn try_from(value: &'a MxcUri) -> Result<Self, Self::Error> {
        Ok(Self::new(value.server_name()?, value.media_id()?)
            .expect("validated MxcUri should always be valid MxcData"))
    }
}

#[cfg(test)]
mod tests {
    use crate::utils::dbg_truncate_str;

    #[test]
    fn test_truncate_str() {
        assert_eq!(dbg_truncate_str("short", 10), "short");
        assert_eq!(dbg_truncate_str("very long string", 10), "very long ...");
        assert_eq!(dbg_truncate_str("no info, only dots", 0), "...");
        assert_eq!(dbg_truncate_str("", 0), "");
        assert_eq!(dbg_truncate_str("unicöde", 5), "unicö...");
        let ok_hand = "👌🏽";
        assert_eq!(dbg_truncate_str(ok_hand, 1), "👌...");
        assert_eq!(dbg_truncate_str(ok_hand, ok_hand.len() - 1), "👌🏽");
        assert_eq!(dbg_truncate_str(ok_hand, ok_hand.len()), "👌🏽");
    }
}
