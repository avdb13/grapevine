use std::net::{IpAddr, Ipv4Addr};

use ruma::{OwnedServerName, RoomVersionId};
use serde::Deserialize;

mod proxy;

use proxy::ProxyConfig;

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Deserialize)]
pub(crate) struct Config {
    #[serde(default = "false_fn")]
    pub(crate) conduit_compat: bool,
    #[serde(default = "default_address")]
    pub(crate) address: IpAddr,
    #[serde(default = "default_port")]
    pub(crate) port: u16,
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
    pub(crate) log: String,
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

fn false_fn() -> bool {
    false
}

fn true_fn() -> bool {
    true
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

fn default_log() -> String {
    "warn,state_res=warn,_=off".to_owned()
}

fn default_turn_ttl() -> u64 {
    60 * 60 * 24
}

// I know, it's a great name
pub(crate) fn default_default_room_version() -> RoomVersionId {
    RoomVersionId::V10
}
