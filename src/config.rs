use std::{
    borrow::Cow,
    fmt::{self, Display},
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
};

use once_cell::sync::Lazy;
use ruma::{OwnedServerName, RoomVersionId};
use serde::Deserialize;

use crate::error;

mod env_filter_clone;
mod proxy;

use env_filter_clone::EnvFilterClone;
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
    pub(crate) database_backend: String,
    pub(crate) database_path: String,
    #[cfg(feature = "rocksdb")]
    #[serde(default = "default_db_cache_capacity_mb")]
    pub(crate) db_cache_capacity_mb: f64,
    #[serde(default = "default_cache_capacity_modifier")]
    pub(crate) cache_capacity_modifier: f64,
    #[cfg(feature = "rocksdb")]
    #[serde(default = "default_rocksdb_max_open_files")]
    pub(crate) rocksdb_max_open_files: i32,
    #[serde(default = "default_pdu_cache_capacity")]
    pub(crate) pdu_cache_capacity: u32,
    #[serde(default = "default_cleanup_second_interval")]
    pub(crate) cleanup_second_interval: u32,
    #[serde(default = "default_max_request_size")]
    pub(crate) max_request_size: u32,
    #[serde(default = "default_max_concurrent_requests")]
    pub(crate) max_concurrent_requests: u16,
    #[serde(default = "default_max_fetch_prev_events")]
    pub(crate) max_fetch_prev_events: u16,
    #[serde(default = "false_fn")]
    pub(crate) allow_registration: bool,
    pub(crate) registration_token: Option<String>,
    #[serde(default = "true_fn")]
    pub(crate) allow_encryption: bool,
    #[serde(default = "true_fn")]
    pub(crate) allow_federation: bool,
    #[serde(default = "true_fn")]
    pub(crate) allow_room_creation: bool,
    #[serde(default = "true_fn")]
    pub(crate) allow_unstable_room_versions: bool,
    #[serde(default = "default_default_room_version")]
    pub(crate) default_room_version: RoomVersionId,
    #[serde(default = "false_fn")]
    pub(crate) allow_jaeger: bool,
    #[serde(default = "false_fn")]
    pub(crate) allow_prometheus: bool,
    #[serde(default = "false_fn")]
    pub(crate) tracing_flame: bool,
    #[serde(default)]
    pub(crate) proxy: ProxyConfig,
    pub(crate) jwt_secret: Option<String>,
    #[serde(default = "default_trusted_servers")]
    pub(crate) trusted_servers: Vec<OwnedServerName>,
    #[serde(default = "default_log")]
    pub(crate) log: EnvFilterClone,
    #[serde(default)]
    pub(crate) turn_username: String,
    #[serde(default)]
    pub(crate) turn_password: String,
    #[serde(default = "Vec::new")]
    pub(crate) turn_uris: Vec<String>,
    #[serde(default)]
    pub(crate) turn_secret: String,
    #[serde(default = "default_turn_ttl")]
    pub(crate) turn_ttl: u64,

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

#[cfg(feature = "rocksdb")]
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

fn default_max_concurrent_requests() -> u16 {
    100
}

fn default_max_fetch_prev_events() -> u16 {
    100_u16
}

fn default_trusted_servers() -> Vec<OwnedServerName> {
    vec![OwnedServerName::try_from("matrix.org").unwrap()]
}

fn default_log() -> EnvFilterClone {
    "warn,state_res=warn,_=off"
        .parse()
        .expect("hardcoded env filter should be valid")
}

fn default_turn_ttl() -> u64 {
    60 * 60 * 24
}

// I know, it's a great name
pub(crate) fn default_default_room_version() -> RoomVersionId {
    RoomVersionId::V10
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
