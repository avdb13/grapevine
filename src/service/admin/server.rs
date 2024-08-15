use toml::Table;

use crate::{args, config, services, utils::query::Query};

#[derive(Debug, clap::Subcommand)]
pub(crate) enum Command {
    /// Show configuration values
    Config,

    // /// Show configuration values
    // ClearCache(Cache),
    /// Print database memory usage statistics
    MemoryUsage,
}

pub(crate) async fn config(query: Query) -> Table {
    let args = args::parse();
    let path = args.config.as_ref();

    let config: Table =
        config::load(path).await.expect("loaded config at startup");

    if query.patterns.is_empty() {
        return config.clone();
    }

    let mut result = Table::new();

    for (key, value) in
        config.into_iter().filter(|(key, _)| query.patterns.contains(key))
    {
        result.insert(key, value);
    }

    result
}

pub(crate) async fn memory_usage() -> (String, String) {
    (services().memory_usage().await, services().globals.db.memory_usage())
}
