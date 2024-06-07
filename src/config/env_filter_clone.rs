//! A workaround for [`EnvFilter`] not directly implementing [`Clone`]
//!
//! This will be unnecessary after [tokio-rs/tracing#2956][0] is merged.
//!
//! [0]: https://github.com/tokio-rs/tracing/pull/2956
#![warn(missing_docs, clippy::missing_docs_in_private_items)]

use std::str::FromStr;

use serde::{de, Deserialize, Deserializer};
use tracing_subscriber::EnvFilter;

/// A workaround for [`EnvFilter`] not directly implementing [`Clone`]
///
/// Use [`FromStr`] or [`Deserialize`] to construct this type, then [`From`] or
/// [`Into`] to convert it into an [`EnvFilter`] when needed.
#[derive(Debug)]
pub(crate) struct EnvFilterClone(String);

impl FromStr for EnvFilterClone {
    type Err = <EnvFilter as FromStr>::Err;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        EnvFilter::from_str(s)?;
        Ok(Self(s.to_owned()))
    }
}

impl From<&EnvFilterClone> for EnvFilter {
    fn from(other: &EnvFilterClone) -> Self {
        EnvFilter::from_str(&other.0)
            .expect("env filter syntax should have been validated already")
    }
}

impl<'de> Deserialize<'de> for EnvFilterClone {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::from_str(&s).map_err(de::Error::custom)
    }
}
