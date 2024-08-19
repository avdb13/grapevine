use std::{
    borrow::Cow,
    fmt::{self, Display},
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
};

use once_cell::sync::Lazy;
use ruma::{OwnedServerName, RoomVersionId};
use serde::{Deserialize, Deserializer};

use crate::error;

mod env_filter_clone;
mod proxy;

pub(crate) use env_filter_clone::EnvFilterClone;
use proxy::ProxyConfig;

/// The default configuration file path
pub(crate) static DEFAULT_PATH: Lazy<PathBuf> =
    Lazy::new(|| [env!("CARGO_PKG_NAME"), "config.toml"].iter().collect());

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Deserialize)]
pub(crate) struct Config {
    #[serde(default = "false_fn")]
    pub(crate) conduit_compat: bool,
    #[serde(default = "default_listen")]
    pub(crate) listen: Vec<ListenConfig>,
    pub(crate) tls: Option<TlsConfig>,

    pub(crate) server_name: OwnedServerName,
    pub(crate) database: DatabaseConfig,
    #[serde(default)]
    pub(crate) federation: FederationConfig,

    #[serde(default = "default_cache_capacity_modifier")]
    pub(crate) cache_capacity_modifier: f64,
    #[serde(default = "default_pdu_cache_capacity")]
    pub(crate) pdu_cache_capacity: u32,
    #[serde(default = "default_cleanup_second_interval")]
    pub(crate) cleanup_second_interval: u32,
    #[serde(default = "default_max_request_size")]
    pub(crate) max_request_size: u32,
    #[serde(default = "false_fn")]
    pub(crate) allow_registration: bool,
    pub(crate) registration_token: Option<String>,
    #[serde(default = "true_fn")]
    pub(crate) allow_encryption: bool,
    #[serde(default = "true_fn")]
    pub(crate) allow_room_creation: bool,
    #[serde(default = "true_fn")]
    pub(crate) allow_unstable_room_versions: bool,
    #[serde(default = "default_default_room_version")]
    pub(crate) default_room_version: RoomVersionId,
    #[serde(default)]
    pub(crate) proxy: ProxyConfig,
    pub(crate) jwt_secret: Option<String>,
    #[serde(default)]
    pub(crate) observability: ObservabilityConfig,
    #[serde(default)]
    pub(crate) turn: TurnConfig,

    pub(crate) emergency_password: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TlsConfig {
    pub(crate) certs: String,
    pub(crate) key: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ListenConfig {
    Tcp {
        #[serde(default = "default_address")]
        address: IpAddr,
        #[serde(default = "default_port")]
        port: u16,
        #[serde(default = "false_fn")]
        tls: bool,
    },
}

impl Display for ListenConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ListenConfig::Tcp {
                address,
                port,
                tls: false,
            } => write!(f, "http://{address}:{port}"),
            ListenConfig::Tcp {
                address,
                port,
                tls: true,
            } => write!(f, "https://{address}:{port}"),
        }
    }
}

#[derive(Copy, Clone, Default, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LogFormat {
    /// Use the [`tracing_subscriber::fmt::format::Pretty`] formatter
    Pretty,
    /// Use the [`tracing_subscriber::fmt::format::Full`] formatter
    #[default]
    Full,
    /// Use the [`tracing_subscriber::fmt::format::Compact`] formatter
    Compact,
    /// Use the [`tracing_subscriber::fmt::format::Json`] formatter
    Json,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub(crate) struct TurnConfig {
    pub(crate) username: String,
    pub(crate) password: String,
    pub(crate) uris: Vec<String>,
    pub(crate) secret: String,
    pub(crate) ttl: u64,
}

impl Default for TurnConfig {
    fn default() -> Self {
        Self {
            username: String::new(),
            password: String::new(),
            uris: Vec::new(),
            secret: String::new(),
            ttl: 60 * 60 * 24,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DatabaseBackend {
    #[cfg(feature = "rocksdb")]
    Rocksdb,
    #[cfg(feature = "sqlite")]
    Sqlite,
}

impl Display for DatabaseBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            #[cfg(feature = "rocksdb")]
            DatabaseBackend::Rocksdb => write!(f, "RocksDB"),
            #[cfg(feature = "sqlite")]
            DatabaseBackend::Sqlite => write!(f, "SQLite"),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct DatabaseConfig {
    pub(crate) backend: DatabaseBackend,
    pub(crate) path: String,
    #[serde(default = "default_db_cache_capacity_mb")]
    pub(crate) cache_capacity_mb: f64,
    #[cfg(feature = "rocksdb")]
    #[serde(default = "default_rocksdb_max_open_files")]
    pub(crate) rocksdb_max_open_files: i32,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct MetricsConfig {
    pub(crate) enable: bool,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub(crate) struct OtelTraceConfig {
    pub(crate) enable: bool,
    pub(crate) filter: EnvFilterClone,
    pub(crate) endpoint: Option<String>,
}

impl Default for OtelTraceConfig {
    fn default() -> Self {
        Self {
            enable: false,
            filter: default_tracing_filter(),
            endpoint: None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub(crate) struct FlameConfig {
    pub(crate) enable: bool,
    pub(crate) filter: EnvFilterClone,
    pub(crate) filename: String,
}

impl Default for FlameConfig {
    fn default() -> Self {
        Self {
            enable: false,
            filter: default_tracing_filter(),
            filename: "./tracing.folded".to_owned(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub(crate) struct LogConfig {
    pub(crate) filter: EnvFilterClone,
    pub(crate) colors: bool,
    pub(crate) format: LogFormat,
    pub(crate) timestamp: bool,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            filter: default_tracing_filter(),
            colors: true,
            format: LogFormat::default(),
            timestamp: true,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ObservabilityConfig {
    /// Prometheus metrics
    pub(crate) metrics: MetricsConfig,
    /// OpenTelemetry traces
    pub(crate) traces: OtelTraceConfig,
    /// Folded inferno stack traces
    pub(crate) flame: FlameConfig,
    /// Logging to stdout
    pub(crate) logs: LogConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub(crate) struct FederationConfig {
    pub(crate) enable: bool,
    pub(crate) trusted_servers: Vec<OwnedServerName>,
    pub(crate) max_fetch_prev_events: u16,
    pub(crate) max_concurrent_requests: u16,
    pub(crate) backoff: BackoffConfig,
}

impl Default for FederationConfig {
    fn default() -> Self {
        Self {
            enable: true,
            trusted_servers: vec![
                OwnedServerName::try_from("matrix.org").unwrap()
            ],
            max_fetch_prev_events: 100,
            max_concurrent_requests: 100,
            backoff: BackoffConfig::default(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub(crate) struct BackoffConfig {
    /// Minimum number of consecutive failures for a server before starting to
    /// delay requests.
    pub(crate) failure_threshold: u8,

    /// Initial delay between requests in seconds, after the number of
    /// consecutive failures to a server first exceeds the threshold.
    pub(crate) base_delay: u32,

    /// Factor to increase delay by after each additional consecutive failure.
    pub(crate) multiplier: f64,

    /// Maximum delay between requests to a server in seconds.
    pub(crate) max_delay: u32,

    /// Range of random multipliers to request delay.
    #[serde(deserialize_with = "deserialize_jitter_range")]
    pub(crate) jitter_range: std::ops::Range<f64>,
}

// TODO: are these reasonable parameters? The 24h max delay was pulled from
// the previous backoff logic for device keys, but it seems quite high
// to me.
impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            base_delay: 5,
            multiplier: 1.5,
            max_delay: 60 * 60 * 24,
            jitter_range: 0.5..1.5,
        }
    }
}

fn false_fn() -> bool {
    false
}

fn true_fn() -> bool {
    true
}

fn default_listen() -> Vec<ListenConfig> {
    vec![ListenConfig::Tcp {
        address: default_address(),
        port: default_port(),
        tls: false,
    }]
}

fn default_address() -> IpAddr {
    Ipv4Addr::LOCALHOST.into()
}

fn default_port() -> u16 {
    6167
}

fn default_db_cache_capacity_mb() -> f64 {
    300.0
}

fn default_cache_capacity_modifier() -> f64 {
    1.0
}

#[cfg(feature = "rocksdb")]
fn default_rocksdb_max_open_files() -> i32 {
    1000
}

fn default_pdu_cache_capacity() -> u32 {
    150_000
}

fn default_cleanup_second_interval() -> u32 {
    // every minute
    60
}

fn default_max_request_size() -> u32 {
    // Default to 20 MB
    20 * 1024 * 1024
}

fn default_tracing_filter() -> EnvFilterClone {
    "info,ruma_state_res=warn"
        .parse()
        .expect("hardcoded env filter should be valid")
}

// I know, it's a great name
pub(crate) fn default_default_room_version() -> RoomVersionId {
    RoomVersionId::V10
}

fn deserialize_jitter_range<'de, D>(
    deserializer: D,
) -> Result<std::ops::Range<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    let Some((a, b)) = s.split_once("..") else {
        return Err(serde::de::Error::custom(crate::Error::bad_config(
            "invalid jitter range",
        )));
    };

    a.parse()
        .and_then(|a| b.parse().map(|b| a..b))
        .map_err(serde::de::Error::custom)
}

/// Search default locations for a configuration file
///
/// If one isn't found, the list of tried paths is returned.
fn search() -> Result<PathBuf, error::ConfigSearch> {
    use error::ConfigSearch as Error;

    xdg::BaseDirectories::new()?
        .find_config_file(&*DEFAULT_PATH)
        .ok_or(Error::NotFound)
}

/// Load the configuration from the given path or XDG Base Directories
pub(crate) async fn load<P>(path: Option<P>) -> Result<Config, error::Config>
where
    P: AsRef<Path>,
{
    use error::Config as Error;

    let path = match path.as_ref().map(AsRef::as_ref) {
        Some(x) => Cow::Borrowed(x),
        None => Cow::Owned(search()?),
    };

    let path = path.as_ref();

    toml::from_str(
        &tokio::fs::read_to_string(path)
            .await
            .map_err(|e| Error::Read(e, path.to_owned()))?,
    )
    .map_err(|e| Error::Parse(e, path.to_owned()))
}
