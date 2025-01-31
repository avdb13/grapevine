mod data;
use std::{
    collections::{BTreeMap, HashMap},
    error::Error as StdError,
    fs,
    future::{self, Future},
    iter,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{
        atomic::{self, AtomicBool},
        Arc, RwLock as StdRwLock,
    },
    time::{Duration, Instant},
};

use base64::{engine::general_purpose, Engine as _};
pub(crate) use data::{Data, SigningKeys};
use futures_util::FutureExt;
use hyper::service::Service as _;
use hyper_util::{
    client::legacy::connect::dns::GaiResolver, service::TowerToHyperService,
};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use ruma::{
    api::federation::discovery::ServerSigningKeys, serde::Base64, DeviceId,
    MilliSecondsSinceUnixEpoch, OwnedEventId, OwnedRoomAliasId, OwnedRoomId,
    OwnedServerName, OwnedUserId, RoomAliasId, RoomVersionId, ServerName,
    UserId,
};
use tokio::sync::{broadcast, Mutex, RwLock, Semaphore};
use tracing::{error, Instrument};
use trust_dns_resolver::TokioAsyncResolver;

use crate::{
    api::server_server::FedDest,
    observability::FilterReloadHandles,
    services,
    utils::on_demand_hashmap::{OnDemandHashMap, TokenSet},
    Config, Error, Result,
};

type WellKnownMap = HashMap<OwnedServerName, (FedDest, String)>;
type TlsNameMap = HashMap<String, (Vec<IpAddr>, u16)>;
// Time if last failed try, number of failed tries
type RateLimitState = (Instant, u32);

// Markers for
// [`Service::roomid_mutex_state`]/[`Service::roomid_mutex_insert`]/
// [`Service::roomid_mutex_federation`]
pub(crate) mod marker {
    pub(crate) enum State {}
    pub(crate) enum Insert {}
    pub(crate) enum Federation {}
}

pub(crate) struct Service {
    pub(crate) db: &'static dyn Data,
    pub(crate) reload_handles: FilterReloadHandles,

    // actual_destination, host
    pub(crate) actual_destination_cache: Arc<RwLock<WellKnownMap>>,
    pub(crate) tls_name_override: Arc<StdRwLock<TlsNameMap>>,
    pub(crate) config: Config,
    keypair: Arc<ruma::signatures::Ed25519KeyPair>,
    dns_resolver: TokioAsyncResolver,
    jwt_decoding_key: Option<jsonwebtoken::DecodingKey>,
    federation_client: reqwest::Client,
    default_client: reqwest::Client,
    pub(crate) stable_room_versions: Vec<RoomVersionId>,
    pub(crate) admin_bot_user_id: OwnedUserId,
    pub(crate) admin_bot_room_alias_id: OwnedRoomAliasId,
    pub(crate) bad_event_ratelimiter:
        Arc<RwLock<HashMap<OwnedEventId, RateLimitState>>>,
    pub(crate) bad_signature_ratelimiter:
        Arc<RwLock<HashMap<Vec<String>, RateLimitState>>>,
    pub(crate) bad_query_ratelimiter:
        Arc<RwLock<HashMap<OwnedServerName, RateLimitState>>>,
    pub(crate) servername_ratelimiter:
        OnDemandHashMap<OwnedServerName, Semaphore>,
    pub(crate) roomid_mutex_insert: TokenSet<OwnedRoomId, marker::Insert>,
    pub(crate) roomid_mutex_state: TokenSet<OwnedRoomId, marker::State>,

    // this lock will be held longer
    pub(crate) roomid_mutex_federation:
        TokenSet<OwnedRoomId, marker::Federation>,
    pub(crate) roomid_federationhandletime:
        RwLock<HashMap<OwnedRoomId, (OwnedEventId, Instant)>>,
    pub(crate) stateres_mutex: Arc<Mutex<()>>,
    pub(crate) rotate: RotationHandler,

    pub(crate) shutdown: AtomicBool,
}

/// Handles "rotation" of long-polling requests. "Rotation" in this context is
/// similar to "rotation" of log files and the like.
///
/// This is utilized to have sync workers return early and release read locks on
/// the database.
pub(crate) struct RotationHandler(
    broadcast::Sender<()>,
    // TODO: Determine if it's safe to delete this field. I'm not deleting it
    // right now because I'm unsure what implications that would have for how
    // the sender expects to work.
    #[allow(dead_code)] broadcast::Receiver<()>,
);

impl RotationHandler {
    pub(crate) fn new() -> Self {
        let (s, r) = broadcast::channel(1);
        Self(s, r)
    }

    pub(crate) fn watch(&self) -> impl Future<Output = ()> {
        let mut r = self.0.subscribe();

        async move {
            r.recv().await.expect("should receive a message");
        }
    }

    pub(crate) fn fire(&self) {
        self.0.send(()).expect("should be able to send message");
    }
}

impl Default for RotationHandler {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) struct Resolver {
    inner: GaiResolver,
    overrides: Arc<StdRwLock<TlsNameMap>>,
}

impl Resolver {
    pub(crate) fn new(overrides: Arc<StdRwLock<TlsNameMap>>) -> Self {
        Resolver {
            inner: GaiResolver::new(),
            overrides,
        }
    }
}

impl Resolve for Resolver {
    #[tracing::instrument(skip(self))]
    fn resolve(&self, name: Name) -> Resolving {
        self.overrides
            .read()
            .unwrap()
            .get(name.as_str())
            .and_then(|(override_name, port)| {
                override_name.first().map(|first_name| {
                    let x: Box<dyn Iterator<Item = SocketAddr> + Send> =
                        Box::new(iter::once(SocketAddr::new(
                            *first_name,
                            *port,
                        )));
                    let x: Resolving = Box::pin(future::ready(Ok(x)));
                    x
                })
            })
            .unwrap_or_else(|| {
                // This should never fail because reqwest's type is a wrapper
                // around hyper-utils' type
                let name = name.as_str().parse().expect("name should be valid");

                Box::pin(
                    TowerToHyperService::new(self.inner.clone())
                        .call(name)
                        .map(|result| {
                            result
                                .map(|addrs| -> Addrs { Box::new(addrs) })
                                .map_err(
                                    |err| -> Box<dyn StdError + Send + Sync> {
                                        Box::new(err)
                                    },
                                )
                        })
                        .in_current_span(),
                )
            })
    }
}

impl Service {
    #[tracing::instrument(skip_all)]
    pub(crate) fn load(
        db: &'static dyn Data,
        config: Config,
        reload_handles: FilterReloadHandles,
    ) -> Result<Self> {
        let keypair = db.load_keypair();

        let keypair = match keypair {
            Ok(k) => k,
            Err(e) => {
                error!("Keypair invalid. Deleting...");
                db.remove_keypair()?;
                return Err(e);
            }
        };

        let tls_name_override = Arc::new(StdRwLock::new(TlsNameMap::new()));

        let jwt_decoding_key = config.jwt_secret.as_ref().map(|secret| {
            jsonwebtoken::DecodingKey::from_secret(secret.as_bytes())
        });

        let default_client = reqwest_client_builder(&config)?.build()?;
        let federation_client = reqwest_client_builder(&config)?
            .dns_resolver(Arc::new(Resolver::new(tls_name_override.clone())))
            .build()?;

        // Supported and stable room versions
        let stable_room_versions = vec![
            RoomVersionId::V6,
            RoomVersionId::V7,
            RoomVersionId::V8,
            RoomVersionId::V9,
            RoomVersionId::V10,
            RoomVersionId::V11,
        ];

        let admin_bot_user_id = UserId::parse(format!(
            "@{}:{}",
            if config.conduit_compat {
                "conduit"
            } else {
                "grapevine"
            },
            config.server_name,
        ))
        .expect("admin bot user ID should be valid");

        let admin_bot_room_alias_id =
            RoomAliasId::parse(format!("#admins:{}", config.server_name))
                .expect("admin bot room alias ID should be valid");

        let mut s = Self {
            db,
            config,
            reload_handles,
            keypair: Arc::new(keypair),
            dns_resolver: TokioAsyncResolver::tokio_from_system_conf()
                .map_err(|e| {
                    error!(
                        "Failed to set up trust dns resolver with system \
                         config: {}",
                        e
                    );
                    Error::bad_config(
                        "Failed to set up trust dns resolver with system \
                         config.",
                    )
                })?,
            actual_destination_cache: Arc::new(
                RwLock::new(WellKnownMap::new()),
            ),
            tls_name_override,
            federation_client,
            default_client,
            jwt_decoding_key,
            stable_room_versions,
            admin_bot_user_id,
            admin_bot_room_alias_id,
            bad_event_ratelimiter: Arc::new(RwLock::new(HashMap::new())),
            bad_signature_ratelimiter: Arc::new(RwLock::new(HashMap::new())),
            bad_query_ratelimiter: Arc::new(RwLock::new(HashMap::new())),
            servername_ratelimiter: OnDemandHashMap::new(
                "servername_ratelimiter".to_owned(),
            ),
            roomid_mutex_state: TokenSet::new("roomid_mutex_state".to_owned()),
            roomid_mutex_insert: TokenSet::new(
                "roomid_mutex_insert".to_owned(),
            ),
            roomid_mutex_federation: TokenSet::new(
                "roomid_mutex_federation".to_owned(),
            ),
            roomid_federationhandletime: RwLock::new(HashMap::new()),
            stateres_mutex: Arc::new(Mutex::new(())),
            rotate: RotationHandler::new(),
            shutdown: AtomicBool::new(false),
        };

        fs::create_dir_all(s.get_media_folder())?;

        if !s.supported_room_versions().contains(&s.config.default_room_version)
        {
            error!(config=?s.config.default_room_version, fallback=?crate::config::default_default_room_version(), "Room version in config isn't supported, falling back to default version");
            s.config.default_room_version =
                crate::config::default_default_room_version();
        };

        Ok(s)
    }

    /// Returns this server's keypair.
    pub(crate) fn keypair(&self) -> &ruma::signatures::Ed25519KeyPair {
        &self.keypair
    }

    /// Returns a reqwest client which can be used to send requests
    pub(crate) fn default_client(&self) -> reqwest::Client {
        // Client is cheap to clone (Arc wrapper) and avoids lifetime issues
        self.default_client.clone()
    }

    /// Returns a client used for resolving .well-knowns
    pub(crate) fn federation_client(&self) -> reqwest::Client {
        // Client is cheap to clone (Arc wrapper) and avoids lifetime issues
        self.federation_client.clone()
    }

    #[tracing::instrument(skip(self))]
    pub(crate) fn next_count(&self) -> Result<u64> {
        self.db.next_count()
    }

    #[tracing::instrument(skip(self))]
    pub(crate) fn current_count(&self) -> Result<u64> {
        self.db.current_count()
    }

    pub(crate) async fn watch(
        &self,
        user_id: &UserId,
        device_id: &DeviceId,
    ) -> Result<()> {
        self.db.watch(user_id, device_id).await
    }

    pub(crate) fn cleanup(&self) -> Result<()> {
        self.db.cleanup()
    }

    pub(crate) fn server_name(&self) -> &ServerName {
        self.config.server_name.as_ref()
    }

    pub(crate) fn max_request_size(&self) -> u32 {
        self.config.max_request_size
    }

    pub(crate) fn max_fetch_prev_events(&self) -> u16 {
        self.config.federation.max_fetch_prev_events
    }

    pub(crate) fn allow_registration(&self) -> bool {
        self.config.allow_registration
    }

    pub(crate) fn allow_encryption(&self) -> bool {
        self.config.allow_encryption
    }

    pub(crate) fn allow_federation(&self) -> bool {
        self.config.federation.enable
    }

    pub(crate) fn allow_room_creation(&self) -> bool {
        self.config.allow_room_creation
    }

    pub(crate) fn default_room_version(&self) -> RoomVersionId {
        self.config.default_room_version.clone()
    }

    pub(crate) fn trusted_servers(&self) -> &[OwnedServerName] {
        &self.config.federation.trusted_servers
    }

    pub(crate) fn dns_resolver(&self) -> &TokioAsyncResolver {
        &self.dns_resolver
    }

    pub(crate) fn jwt_decoding_key(
        &self,
    ) -> Option<&jsonwebtoken::DecodingKey> {
        self.jwt_decoding_key.as_ref()
    }

    pub(crate) fn turn_password(&self) -> &String {
        &self.config.turn.password
    }

    pub(crate) fn turn_ttl(&self) -> u64 {
        self.config.turn.ttl
    }

    pub(crate) fn turn_uris(&self) -> &[String] {
        &self.config.turn.uris
    }

    pub(crate) fn turn_username(&self) -> &String {
        &self.config.turn.username
    }

    pub(crate) fn turn_secret(&self) -> &String {
        &self.config.turn.secret
    }

    pub(crate) fn emergency_password(&self) -> &Option<String> {
        &self.config.emergency_password
    }

    pub(crate) fn supported_room_versions(&self) -> Vec<RoomVersionId> {
        self.stable_room_versions.clone()
    }

    /// This doesn't actually check that the keys provided are newer than the
    /// old set.
    pub(crate) fn add_signing_key_from_trusted_server(
        &self,
        origin: &ServerName,
        new_keys: ServerSigningKeys,
    ) -> Result<SigningKeys> {
        self.db.add_signing_key_from_trusted_server(origin, new_keys)
    }

    /// Same as `from_trusted_server`, except it will move active keys not
    /// present in `new_keys` to `old_signing_keys`
    pub(crate) fn add_signing_key_from_origin(
        &self,
        origin: &ServerName,
        new_keys: ServerSigningKeys,
    ) -> Result<SigningKeys> {
        self.db.add_signing_key_from_origin(origin, new_keys)
    }

    /// This returns Ok(None) when there are no keys found for the server.
    pub(crate) fn signing_keys_for(
        &self,
        origin: &ServerName,
    ) -> Result<Option<SigningKeys>> {
        Ok(self.db.signing_keys_for(origin)?.or_else(|| {
            (origin == self.server_name()).then(SigningKeys::load_own_keys)
        }))
    }

    /// Filters the key map of multiple servers down to keys that should be
    /// accepted given the expiry time, room version, and timestamp of the
    /// paramters
    #[allow(clippy::unused_self)]
    pub(crate) fn filter_keys_server_map(
        &self,
        keys: BTreeMap<String, SigningKeys>,
        timestamp: MilliSecondsSinceUnixEpoch,
        room_version_id: &RoomVersionId,
    ) -> BTreeMap<String, BTreeMap<String, Base64>> {
        keys.into_iter()
            .filter_map(|(server, keys)| {
                self.filter_keys_single_server(keys, timestamp, room_version_id)
                    .map(|keys| (server, keys))
            })
            .collect()
    }

    /// Filters the keys of a single server down to keys that should be accepted
    /// given the expiry time, room version, and timestamp of the paramters
    #[allow(clippy::unused_self)]
    pub(crate) fn filter_keys_single_server(
        &self,
        keys: SigningKeys,
        timestamp: MilliSecondsSinceUnixEpoch,
        _room_version_id: &RoomVersionId,
    ) -> Option<BTreeMap<String, Base64>> {
        let all_valid = keys.valid_until_ts > timestamp;

        all_valid.then(|| {
            // Given that either the room version allows stale keys, or the
            // valid_until_ts is in the future, all verify_keys are
            // valid
            let mut map: BTreeMap<_, _> = keys
                .verify_keys
                .into_iter()
                .map(|(id, key)| (id, key.key))
                .collect();

            map.extend(keys.old_verify_keys.into_iter().filter_map(
                |(id, key)| {
                    // Even on old room versions, we don't allow old keys if
                    // they are expired
                    (key.expired_ts > timestamp).then_some((id, key.key))
                },
            ));

            map
        })
    }

    pub(crate) fn database_version(&self) -> Result<u64> {
        self.db.database_version()
    }

    pub(crate) fn bump_database_version(&self, new_version: u64) -> Result<()> {
        self.db.bump_database_version(new_version)
    }

    pub(crate) fn get_media_folder(&self) -> PathBuf {
        let mut r = PathBuf::new();
        r.push(self.config.database.path.clone());
        r.push("media");
        r
    }

    pub(crate) fn get_media_file(&self, key: &[u8]) -> PathBuf {
        let mut r = PathBuf::new();
        r.push(self.config.database.path.clone());
        r.push("media");
        r.push(general_purpose::URL_SAFE_NO_PAD.encode(key));
        r
    }

    pub(crate) fn shutdown(&self) {
        self.shutdown.store(true, atomic::Ordering::Relaxed);
        services().globals.rotate.fire();
    }
}

fn reqwest_client_builder(config: &Config) -> Result<reqwest::ClientBuilder> {
    let mut reqwest_client_builder = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(60 * 3));

    if let Some(proxy) = config.proxy.to_proxy()? {
        reqwest_client_builder = reqwest_client_builder.proxy(proxy);
    }

    Ok(reqwest_client_builder)
}
