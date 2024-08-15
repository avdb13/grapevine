use std::{collections::HashSet, hash::Hash, ops::Deref, str::FromStr};

use clap::Args;
use ruma::{self, api::Direction};
use serde::Deserialize;

use crate::Result;

#[derive(Clone, Debug, Args)]
pub(crate) struct Query {
    pub(crate) patterns: Values<String>,
    #[arg(value_parser = clap::value_parser!(String))]
    pub(crate) direction: Direction,
    #[arg(default_value = "0")]
    pub(crate) offset: usize,
    #[arg(default_value = "10")]
    pub(crate) limit: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct Values<T>(HashSet<T>);

impl<T> FromStr for Values<T>
where
    T: Eq + Hash + for<'de> Deserialize<'de>,
{
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let split: Result<_, _> =
            s.split(',').map(str::trim).map(serde_json::from_str).collect();

        split.map(Self)
    }
}

impl<T> Deref for Values<T> {
    type Target = HashSet<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
