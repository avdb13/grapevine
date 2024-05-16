use std::{
    collections::BTreeMap,
    fmt,
    fmt::Write,
    net::{IpAddr, Ipv4Addr},
};

use ruma::{OwnedServerName, RoomVersionId};
use serde::{de::IgnoredAny, Deserialize};
use tracing::warn;

mod proxy;

use self::proxy::ProxyConfig;

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Deserialize)]
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
    #[serde(default = "false_fn")]
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

    #[serde(flatten)]
    // This has special meaning to `serde`
    #[allow(clippy::zero_sized_map_values)]
    pub(crate) catchall: BTreeMap<String, IgnoredAny>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct TlsConfig {
    pub(crate) certs: String,
    pub(crate) key: String,
}

const DEPRECATED_KEYS: &[&str] = &["cache_capacity"];

impl Config {
    pub(crate) fn warn_deprecated(&self) {
        let mut was_deprecated = false;
        for key in self
            .catchall
            .keys()
            .filter(|key| DEPRECATED_KEYS.iter().any(|s| s == key))
        {
            warn!("Config parameter {} is deprecated", key);
            was_deprecated = true;
        }

        if was_deprecated {
            warn!(
                "Read grapevine documentation and check your configuration if \
                 any new configuration parameters should be adjusted"
            );
        }
    }
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Prepare a list of config values to show
        let lines = [
            ("Server name", self.server_name.host()),
            ("Database backend", &self.database_backend),
            ("Database path", &self.database_path),
            (
                "Database cache capacity (MB)",
                &self.db_cache_capacity_mb.to_string(),
            ),
            (
                "Cache capacity modifier",
                &self.cache_capacity_modifier.to_string(),
            ),
            #[cfg(feature = "rocksdb")]
            (
                "Maximum open files for RocksDB",
                &self.rocksdb_max_open_files.to_string(),
            ),
            ("PDU cache capacity", &self.pdu_cache_capacity.to_string()),
            (
                "Cleanup interval in seconds",
                &self.cleanup_second_interval.to_string(),
            ),
            ("Maximum request size", &self.max_request_size.to_string()),
            (
                "Maximum concurrent requests",
                &self.max_concurrent_requests.to_string(),
            ),
            ("Allow registration", &self.allow_registration.to_string()),
            ("Allow encryption", &self.allow_encryption.to_string()),
            ("Allow federation", &self.allow_federation.to_string()),
            ("Allow room creation", &self.allow_room_creation.to_string()),
            (
                "JWT secret",
                match self.jwt_secret {
                    Some(_) => "set",
                    None => "not set",
                },
            ),
            ("Trusted servers", {
                let mut lst = vec![];
                for server in &self.trusted_servers {
                    lst.push(server.host());
                }
                &lst.join(", ")
            }),
            (
                "TURN username",
                if self.turn_username.is_empty() {
                    "not set"
                } else {
                    &self.turn_username
                },
            ),
            ("TURN password", {
                if self.turn_password.is_empty() {
                    "not set"
                } else {
                    "set"
                }
            }),
            ("TURN secret", {
                if self.turn_secret.is_empty() {
                    "not set"
                } else {
                    "set"
                }
            }),
            ("Turn TTL", &self.turn_ttl.to_string()),
            ("Turn URIs", {
                let mut lst = vec![];
                for item in self.turn_uris.iter().cloned().enumerate() {
                    let (_, uri): (usize, String) = item;
                    lst.push(uri);
                }
                &lst.join(", ")
            }),
        ];

        let mut msg: String = "Active config values:\n\n".to_owned();

        for line in lines.into_iter().enumerate() {
            writeln!(msg, "{}: {}", line.1 .0, line.1 .1)
                .expect("write to in-memory buffer should succeed");
        }

        write!(f, "{msg}")
    }
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
