pub(crate) mod abstraction;
pub(crate) mod key_value;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::Write,
    mem::size_of,
    path::Path,
    sync::{Arc, Mutex, RwLock},
};

use abstraction::{KeyValueDatabaseEngine, KvTree};
use lru_cache::LruCache;
use ruma::{
    events::{
        push_rules::{PushRulesEvent, PushRulesEventContent},
        room::message::RoomMessageEventContent,
        GlobalAccountDataEvent, GlobalAccountDataEventType, StateEventType,
    },
    push::Ruleset,
    CanonicalJsonValue, EventId, OwnedDeviceId, OwnedEventId, OwnedRoomId,
    OwnedUserId, RoomId, UserId,
};
use tracing::{debug, error, info, info_span, warn, Instrument};

use crate::{
    config::DatabaseBackend, observability::FilterReloadHandles,
    service::rooms::timeline::PduCount, services, utils, Config, Error,
    PduEvent, Result, Services, SERVICES,
};

pub(crate) struct KeyValueDatabase {
    db: Arc<dyn KeyValueDatabaseEngine>,

    // Trees "owned" by `self::key_value::globals`
    pub(super) global: Arc<dyn KvTree>,
    pub(super) server_signingkeys: Arc<dyn KvTree>,

    // Trees "owned" by `self::key_value::users`
    pub(super) userid_password: Arc<dyn KvTree>,
    pub(super) userid_displayname: Arc<dyn KvTree>,
    pub(super) userid_avatarurl: Arc<dyn KvTree>,
    pub(super) userid_blurhash: Arc<dyn KvTree>,
    pub(super) userdeviceid_token: Arc<dyn KvTree>,

    // This is also used to check if a device exists
    pub(super) userdeviceid_metadata: Arc<dyn KvTree>,

    // DevicelistVersion = u64
    pub(super) userid_devicelistversion: Arc<dyn KvTree>,
    pub(super) token_userdeviceid: Arc<dyn KvTree>,

    // OneTimeKeyId = UserId + DeviceKeyId
    pub(super) onetimekeyid_onetimekeys: Arc<dyn KvTree>,

    // LastOneTimeKeyUpdate = Count
    pub(super) userid_lastonetimekeyupdate: Arc<dyn KvTree>,

    // KeyChangeId = UserId/RoomId + Count
    pub(super) keychangeid_userid: Arc<dyn KvTree>,

    // KeyId = UserId + KeyId (depends on key type)
    pub(super) keyid_key: Arc<dyn KvTree>,
    pub(super) userid_masterkeyid: Arc<dyn KvTree>,
    pub(super) userid_selfsigningkeyid: Arc<dyn KvTree>,
    pub(super) userid_usersigningkeyid: Arc<dyn KvTree>,

    // UserFilterId = UserId + FilterId
    pub(super) userfilterid_filter: Arc<dyn KvTree>,

    // ToDeviceId = UserId + DeviceId + Count
    pub(super) todeviceid_events: Arc<dyn KvTree>,

    // Trees "owned" by `self::key_value::uiaa`
    // User-interactive authentication
    pub(super) userdevicesessionid_uiaainfo: Arc<dyn KvTree>,
    pub(super) userdevicesessionid_uiaarequest: RwLock<
        BTreeMap<(OwnedUserId, OwnedDeviceId, String), CanonicalJsonValue>,
    >,

    // Trees "owned" by `self::key_value::rooms::edus`
    // ReadReceiptId = RoomId + Count + UserId
    pub(super) readreceiptid_readreceipt: Arc<dyn KvTree>,

    // RoomUserId = Room + User, PrivateRead = Count
    pub(super) roomuserid_privateread: Arc<dyn KvTree>,

    // LastPrivateReadUpdate = Count
    pub(super) roomuserid_lastprivatereadupdate: Arc<dyn KvTree>,

    // PresenceId = RoomId + Count + UserId
    // This exists in the database already but is currently unused
    #[allow(dead_code)]
    pub(super) presenceid_presence: Arc<dyn KvTree>,

    // LastPresenceUpdate = Count
    // This exists in the database already but is currently unused
    #[allow(dead_code)]
    pub(super) userid_lastpresenceupdate: Arc<dyn KvTree>,

    // Trees "owned" by `self::key_value::rooms`
    // PduId = ShortRoomId + Count
    pub(super) pduid_pdu: Arc<dyn KvTree>,
    pub(super) eventid_pduid: Arc<dyn KvTree>,
    pub(super) roomid_pduleaves: Arc<dyn KvTree>,
    pub(super) alias_roomid: Arc<dyn KvTree>,

    // AliasId = RoomId + Count
    pub(super) aliasid_alias: Arc<dyn KvTree>,
    pub(super) publicroomids: Arc<dyn KvTree>,

    // ThreadId = RoomId + Count
    pub(super) threadid_userids: Arc<dyn KvTree>,

    // TokenId = ShortRoomId + Token + PduIdCount
    pub(super) tokenids: Arc<dyn KvTree>,

    /// Participating servers in a room.
    // RoomServerId = RoomId + ServerName
    pub(super) roomserverids: Arc<dyn KvTree>,

    // ServerRoomId = ServerName + RoomId
    pub(super) serverroomids: Arc<dyn KvTree>,

    pub(super) userroomid_joined: Arc<dyn KvTree>,
    pub(super) roomuserid_joined: Arc<dyn KvTree>,
    pub(super) roomid_joinedcount: Arc<dyn KvTree>,
    pub(super) roomid_invitedcount: Arc<dyn KvTree>,
    pub(super) roomuseroncejoinedids: Arc<dyn KvTree>,

    // InviteState = Vec<Raw<Pdu>>
    pub(super) userroomid_invitestate: Arc<dyn KvTree>,

    // InviteCount = Count
    pub(super) roomuserid_invitecount: Arc<dyn KvTree>,
    pub(super) userroomid_leftstate: Arc<dyn KvTree>,
    pub(super) roomuserid_leftcount: Arc<dyn KvTree>,

    // Rooms where incoming federation handling is disabled
    pub(super) disabledroomids: Arc<dyn KvTree>,

    // LazyLoadedIds = UserId + DeviceId + RoomId + LazyLoadedUserId
    pub(super) lazyloadedids: Arc<dyn KvTree>,

    // NotifyCount = u64
    pub(super) userroomid_notificationcount: Arc<dyn KvTree>,

    // HightlightCount = u64
    pub(super) userroomid_highlightcount: Arc<dyn KvTree>,

    // LastNotificationRead = u64
    pub(super) roomuserid_lastnotificationread: Arc<dyn KvTree>,

    /// Remember the current state hash of a room.
    pub(super) roomid_shortstatehash: Arc<dyn KvTree>,

    pub(super) roomsynctoken_shortstatehash: Arc<dyn KvTree>,

    /// Remember the state hash at events in the past.
    pub(super) shorteventid_shortstatehash: Arc<dyn KvTree>,

    /// `StateKey = EventType + StateKey`, `ShortStateKey = Count`
    pub(super) statekey_shortstatekey: Arc<dyn KvTree>,
    pub(super) shortstatekey_statekey: Arc<dyn KvTree>,

    pub(super) roomid_shortroomid: Arc<dyn KvTree>,

    pub(super) shorteventid_eventid: Arc<dyn KvTree>,
    pub(super) eventid_shorteventid: Arc<dyn KvTree>,

    pub(super) statehash_shortstatehash: Arc<dyn KvTree>,

    // StateDiff = parent (or 0) + (shortstatekey+shorteventid++) + 0_u64 +
    // (shortstatekey+shorteventid--)
    pub(super) shortstatehash_statediff: Arc<dyn KvTree>,

    pub(super) shorteventid_authchain: Arc<dyn KvTree>,

    /// `RoomId + EventId -> outlier PDU`
    ///
    /// Any pdu that has passed the steps 1-8 in the incoming event
    /// /federation/send/txn.
    pub(super) eventid_outlierpdu: Arc<dyn KvTree>,
    pub(super) softfailedeventids: Arc<dyn KvTree>,

    /// `ShortEventId + ShortEventId -> ()`
    pub(super) tofrom_relation: Arc<dyn KvTree>,

    /// `RoomId + EventId -> Parent PDU EventId`
    pub(super) referencedevents: Arc<dyn KvTree>,

    // Trees "owned" by `self::key_value::account_data`
    // RoomUserDataId = Room + User + Count + Type
    pub(super) roomuserdataid_accountdata: Arc<dyn KvTree>,

    // RoomUserType = Room + User + Type
    pub(super) roomusertype_roomuserdataid: Arc<dyn KvTree>,

    // Trees "owned" by `self::key_value::media`
    // MediaId = MXC + WidthHeight + ContentDisposition + ContentType
    pub(super) mediaid_file: Arc<dyn KvTree>,

    // Trees "owned" by `self::key_value::key_backups`
    // BackupId = UserId + Version(Count)
    pub(super) backupid_algorithm: Arc<dyn KvTree>,

    // BackupId = UserId + Version(Count)
    pub(super) backupid_etag: Arc<dyn KvTree>,

    // BackupKeyId = UserId + Version + RoomId + SessionId
    pub(super) backupkeyid_backup: Arc<dyn KvTree>,

    // Trees "owned" by `self::key_value::transaction_ids`
    // Response can be empty (/sendToDevice) or the event id (/send)
    pub(super) userdevicetxnid_response: Arc<dyn KvTree>,

    // Trees "owned" by `self::key_value::sending`
    // EduCount: Count of last EDU sync
    pub(super) servername_educount: Arc<dyn KvTree>,

    // ServernameEvent = (+ / $)SenderKey / ServerName / UserId + PduId / Id
    // (for edus), Data = EDU content
    pub(super) servernameevent_data: Arc<dyn KvTree>,

    // ServerCurrentEvents = (+ / $)ServerName / UserId + PduId / Id (for
    // edus), Data = EDU content
    pub(super) servercurrentevent_data: Arc<dyn KvTree>,

    // Trees "owned" by `self::key_value::appservice`
    pub(super) id_appserviceregistrations: Arc<dyn KvTree>,

    // Trees "owned" by `self::key_value::pusher`
    pub(super) senderkey_pusher: Arc<dyn KvTree>,

    // Uncategorized trees
    pub(super) pdu_cache: Mutex<LruCache<OwnedEventId, Arc<PduEvent>>>,
    pub(super) shorteventid_cache: Mutex<LruCache<u64, Arc<EventId>>>,
    pub(super) auth_chain_cache: Mutex<LruCache<Vec<u64>, Arc<HashSet<u64>>>>,
    pub(super) eventidshort_cache: Mutex<LruCache<OwnedEventId, u64>>,
    pub(super) statekeyshort_cache:
        Mutex<LruCache<(StateEventType, String), u64>>,
    pub(super) shortstatekey_cache:
        Mutex<LruCache<u64, (StateEventType, String)>>,
    pub(super) our_real_users_cache:
        RwLock<HashMap<OwnedRoomId, Arc<HashSet<OwnedUserId>>>>,
    pub(super) appservice_in_room_cache:
        RwLock<HashMap<OwnedRoomId, HashMap<String, bool>>>,
    pub(super) lasttimelinecount_cache: Mutex<HashMap<OwnedRoomId, PduCount>>,
}

impl KeyValueDatabase {
    fn check_db_setup(config: &Config) -> Result<()> {
        let path = Path::new(&config.database.path);

        let sqlite_exists = path
            .join(format!(
                "{}.db",
                if config.conduit_compat {
                    "conduit"
                } else {
                    "grapevine"
                }
            ))
            .exists();
        let rocksdb_exists = path.join("IDENTITY").exists();

        let mut count = 0;

        if sqlite_exists {
            count += 1;
        }

        if rocksdb_exists {
            count += 1;
        }

        if count > 1 {
            warn!("Multiple databases at database_path detected");
            return Ok(());
        }

        let (backend_is_rocksdb, backend_is_sqlite): (bool, bool) =
            match config.database.backend {
                #[cfg(feature = "rocksdb")]
                DatabaseBackend::Rocksdb => (true, false),
                #[cfg(feature = "sqlite")]
                DatabaseBackend::Sqlite => (false, true),
            };

        if sqlite_exists && !backend_is_sqlite {
            return Err(Error::bad_config(
                "Found sqlite at database_path, but is not specified in \
                 config.",
            ));
        }

        if rocksdb_exists && !backend_is_rocksdb {
            return Err(Error::bad_config(
                "Found rocksdb at database_path, but is not specified in \
                 config.",
            ));
        }

        Ok(())
    }

    /// Load an existing database or create a new one.
    #[cfg_attr(
        not(any(feature = "rocksdb", feature = "sqlite")),
        allow(unreachable_code)
    )]
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn load_or_create(
        config: Config,
        reload_handles: FilterReloadHandles,
    ) -> Result<()> {
        Self::check_db_setup(&config)?;

        if !Path::new(&config.database.path).exists() {
            fs::create_dir_all(&config.database.path).map_err(|_| {
                Error::BadConfig(
                    "Database folder doesn't exists and couldn't be created \
                     (e.g. due to missing permissions). Please create the \
                     database folder yourself.",
                )
            })?;
        }

        #[cfg_attr(
            not(any(feature = "rocksdb", feature = "sqlite")),
            allow(unused_variables)
        )]
        let builder: Arc<dyn KeyValueDatabaseEngine> = match config
            .database
            .backend
        {
            #[cfg(feature = "sqlite")]
            DatabaseBackend::Sqlite => {
                Arc::new(Arc::<abstraction::sqlite::Engine>::open(&config)?)
            }
            #[cfg(feature = "rocksdb")]
            DatabaseBackend::Rocksdb => {
                Arc::new(Arc::<abstraction::rocksdb::Engine>::open(&config)?)
            }
        };

        if config.registration_token == Some(String::new()) {
            return Err(Error::bad_config("Registration token is empty"));
        }

        if config.max_request_size < 1024 {
            error!(
                ?config.max_request_size,
                "Max request size is less than 1KB. Please increase it.",
            );
        }

        let db_raw = Box::new(Self {
            db: builder.clone(),
            userid_password: builder.open_tree("userid_password")?,
            userid_displayname: builder.open_tree("userid_displayname")?,
            userid_avatarurl: builder.open_tree("userid_avatarurl")?,
            userid_blurhash: builder.open_tree("userid_blurhash")?,
            userdeviceid_token: builder.open_tree("userdeviceid_token")?,
            userdeviceid_metadata: builder
                .open_tree("userdeviceid_metadata")?,
            userid_devicelistversion: builder
                .open_tree("userid_devicelistversion")?,
            token_userdeviceid: builder.open_tree("token_userdeviceid")?,
            onetimekeyid_onetimekeys: builder
                .open_tree("onetimekeyid_onetimekeys")?,
            userid_lastonetimekeyupdate: builder
                .open_tree("userid_lastonetimekeyupdate")?,
            keychangeid_userid: builder.open_tree("keychangeid_userid")?,
            keyid_key: builder.open_tree("keyid_key")?,
            userid_masterkeyid: builder.open_tree("userid_masterkeyid")?,
            userid_selfsigningkeyid: builder
                .open_tree("userid_selfsigningkeyid")?,
            userid_usersigningkeyid: builder
                .open_tree("userid_usersigningkeyid")?,
            userfilterid_filter: builder.open_tree("userfilterid_filter")?,
            todeviceid_events: builder.open_tree("todeviceid_events")?,

            userdevicesessionid_uiaainfo: builder
                .open_tree("userdevicesessionid_uiaainfo")?,
            userdevicesessionid_uiaarequest: RwLock::new(BTreeMap::new()),
            readreceiptid_readreceipt: builder
                .open_tree("readreceiptid_readreceipt")?,
            // "Private" read receipt
            roomuserid_privateread: builder
                .open_tree("roomuserid_privateread")?,
            roomuserid_lastprivatereadupdate: builder
                .open_tree("roomuserid_lastprivatereadupdate")?,
            presenceid_presence: builder.open_tree("presenceid_presence")?,
            userid_lastpresenceupdate: builder
                .open_tree("userid_lastpresenceupdate")?,
            pduid_pdu: builder.open_tree("pduid_pdu")?,
            eventid_pduid: builder.open_tree("eventid_pduid")?,
            roomid_pduleaves: builder.open_tree("roomid_pduleaves")?,

            alias_roomid: builder.open_tree("alias_roomid")?,
            aliasid_alias: builder.open_tree("aliasid_alias")?,
            publicroomids: builder.open_tree("publicroomids")?,

            threadid_userids: builder.open_tree("threadid_userids")?,

            tokenids: builder.open_tree("tokenids")?,

            roomserverids: builder.open_tree("roomserverids")?,
            serverroomids: builder.open_tree("serverroomids")?,
            userroomid_joined: builder.open_tree("userroomid_joined")?,
            roomuserid_joined: builder.open_tree("roomuserid_joined")?,
            roomid_joinedcount: builder.open_tree("roomid_joinedcount")?,
            roomid_invitedcount: builder.open_tree("roomid_invitedcount")?,
            roomuseroncejoinedids: builder
                .open_tree("roomuseroncejoinedids")?,
            userroomid_invitestate: builder
                .open_tree("userroomid_invitestate")?,
            roomuserid_invitecount: builder
                .open_tree("roomuserid_invitecount")?,
            userroomid_leftstate: builder.open_tree("userroomid_leftstate")?,
            roomuserid_leftcount: builder.open_tree("roomuserid_leftcount")?,

            disabledroomids: builder.open_tree("disabledroomids")?,

            lazyloadedids: builder.open_tree("lazyloadedids")?,

            userroomid_notificationcount: builder
                .open_tree("userroomid_notificationcount")?,
            userroomid_highlightcount: builder
                .open_tree("userroomid_highlightcount")?,
            roomuserid_lastnotificationread: builder
                .open_tree("userroomid_highlightcount")?,

            statekey_shortstatekey: builder
                .open_tree("statekey_shortstatekey")?,
            shortstatekey_statekey: builder
                .open_tree("shortstatekey_statekey")?,

            shorteventid_authchain: builder
                .open_tree("shorteventid_authchain")?,

            roomid_shortroomid: builder.open_tree("roomid_shortroomid")?,

            shortstatehash_statediff: builder
                .open_tree("shortstatehash_statediff")?,
            eventid_shorteventid: builder.open_tree("eventid_shorteventid")?,
            shorteventid_eventid: builder.open_tree("shorteventid_eventid")?,
            shorteventid_shortstatehash: builder
                .open_tree("shorteventid_shortstatehash")?,
            roomid_shortstatehash: builder
                .open_tree("roomid_shortstatehash")?,
            roomsynctoken_shortstatehash: builder
                .open_tree("roomsynctoken_shortstatehash")?,
            statehash_shortstatehash: builder
                .open_tree("statehash_shortstatehash")?,

            eventid_outlierpdu: builder.open_tree("eventid_outlierpdu")?,
            softfailedeventids: builder.open_tree("softfailedeventids")?,

            tofrom_relation: builder.open_tree("tofrom_relation")?,
            referencedevents: builder.open_tree("referencedevents")?,
            roomuserdataid_accountdata: builder
                .open_tree("roomuserdataid_accountdata")?,
            roomusertype_roomuserdataid: builder
                .open_tree("roomusertype_roomuserdataid")?,
            mediaid_file: builder.open_tree("mediaid_file")?,
            backupid_algorithm: builder.open_tree("backupid_algorithm")?,
            backupid_etag: builder.open_tree("backupid_etag")?,
            backupkeyid_backup: builder.open_tree("backupkeyid_backup")?,
            userdevicetxnid_response: builder
                .open_tree("userdevicetxnid_response")?,
            servername_educount: builder.open_tree("servername_educount")?,
            servernameevent_data: builder.open_tree("servernameevent_data")?,
            servercurrentevent_data: builder
                .open_tree("servercurrentevent_data")?,
            id_appserviceregistrations: builder
                .open_tree("id_appserviceregistrations")?,
            senderkey_pusher: builder.open_tree("senderkey_pusher")?,
            global: builder.open_tree("global")?,
            server_signingkeys: builder.open_tree("server_signingkeys")?,

            pdu_cache: Mutex::new(LruCache::new(
                config
                    .pdu_cache_capacity
                    .try_into()
                    .expect("pdu cache capacity fits into usize"),
            )),
            #[allow(
                clippy::as_conversions,
                clippy::cast_sign_loss,
                clippy::cast_possible_truncation
            )]
            auth_chain_cache: Mutex::new(LruCache::new(
                (100_000.0 * config.cache_capacity_modifier) as usize,
            )),
            #[allow(
                clippy::as_conversions,
                clippy::cast_sign_loss,
                clippy::cast_possible_truncation
            )]
            shorteventid_cache: Mutex::new(LruCache::new(
                (100_000.0 * config.cache_capacity_modifier) as usize,
            )),
            #[allow(
                clippy::as_conversions,
                clippy::cast_sign_loss,
                clippy::cast_possible_truncation
            )]
            eventidshort_cache: Mutex::new(LruCache::new(
                (100_000.0 * config.cache_capacity_modifier) as usize,
            )),
            #[allow(
                clippy::as_conversions,
                clippy::cast_sign_loss,
                clippy::cast_possible_truncation
            )]
            shortstatekey_cache: Mutex::new(LruCache::new(
                (100_000.0 * config.cache_capacity_modifier) as usize,
            )),
            #[allow(
                clippy::as_conversions,
                clippy::cast_sign_loss,
                clippy::cast_possible_truncation
            )]
            statekeyshort_cache: Mutex::new(LruCache::new(
                (100_000.0 * config.cache_capacity_modifier) as usize,
            )),
            our_real_users_cache: RwLock::new(HashMap::new()),
            appservice_in_room_cache: RwLock::new(HashMap::new()),
            lasttimelinecount_cache: Mutex::new(HashMap::new()),
        });

        let db = Box::leak(db_raw);

        let services_raw =
            Box::new(Services::build(db, config, reload_handles)?);

        // This is the first and only time we initialize the SERVICE static
        *SERVICES.write().unwrap() = Some(Box::leak(services_raw));

        // Matrix resource ownership is based on the server name; changing it
        // requires recreating the database from scratch.
        if services().users.count()? > 0 {
            let admin_bot = services().globals.admin_bot_user_id.as_ref();
            if !services().users.exists(admin_bot)? {
                error!(
                    user_id = %admin_bot,
                    "The admin bot does not exist and the database is not new",
                );
                return Err(Error::bad_database(
                    "Cannot reuse an existing database after changing the \
                     server name, please delete the old one first.",
                ));
            }
        }

        // If the database has any data, perform data migrations before starting
        let latest_database_version = 13;

        if services().users.count()? > 0 {
            // MIGRATIONS
            if services().globals.database_version()? < 1 {
                for (roomserverid, _) in db.roomserverids.iter() {
                    let mut parts = roomserverid.split(|&b| b == 0xFF);
                    let room_id =
                        parts.next().expect("split always returns one element");
                    let Some(servername) = parts.next() else {
                        error!("Migration: Invalid roomserverid in db.");
                        continue;
                    };
                    let mut serverroomid = servername.to_vec();
                    serverroomid.push(0xFF);
                    serverroomid.extend_from_slice(room_id);

                    db.serverroomids.insert(&serverroomid, &[])?;
                }

                services().globals.bump_database_version(1)?;

                warn!("Migration: 0 -> 1 finished");
            }

            if services().globals.database_version()? < 2 {
                // We accidentally inserted hashed versions of "" into the db
                // instead of just ""
                for (userid, password) in db.userid_password.iter() {
                    let password = utils::string_from_bytes(&password);

                    let empty_hashed_password = password
                        .map_or(false, |password| {
                            utils::verify_password("", password)
                        });

                    if empty_hashed_password {
                        db.userid_password.insert(&userid, b"")?;
                    }
                }

                services().globals.bump_database_version(2)?;

                warn!("Migration: 1 -> 2 finished");
            }

            if services().globals.database_version()? < 3 {
                // Move media to filesystem
                for (key, content) in db.mediaid_file.iter() {
                    if content.is_empty() {
                        continue;
                    }

                    let path = services().globals.get_media_file(&key);
                    let mut file = fs::File::create(path)?;
                    file.write_all(&content)?;
                    db.mediaid_file.insert(&key, &[])?;
                }

                services().globals.bump_database_version(3)?;

                warn!("Migration: 2 -> 3 finished");
            }

            if services().globals.database_version()? < 4 {
                // Add federated users to services() as deactivated
                for our_user in services().users.iter() {
                    let our_user = our_user?;
                    if services().users.is_deactivated(&our_user)? {
                        continue;
                    }
                    for room in
                        services().rooms.state_cache.rooms_joined(&our_user)
                    {
                        for user in
                            services().rooms.state_cache.room_members(&room?)
                        {
                            let user = user?;
                            if user.server_name()
                                != services().globals.server_name()
                            {
                                info!(?user, "Migration: creating user");
                                services().users.create(&user, None)?;
                            }
                        }
                    }
                }

                services().globals.bump_database_version(4)?;

                warn!("Migration: 3 -> 4 finished");
            }

            if services().globals.database_version()? < 5 {
                // Upgrade user data store
                for (roomuserdataid, _) in db.roomuserdataid_accountdata.iter()
                {
                    let mut parts = roomuserdataid.split(|&b| b == 0xFF);
                    let room_id = parts.next().unwrap();
                    let user_id = parts.next().unwrap();
                    let event_type =
                        roomuserdataid.rsplit(|&b| b == 0xFF).next().unwrap();

                    let mut key = room_id.to_vec();
                    key.push(0xFF);
                    key.extend_from_slice(user_id);
                    key.push(0xFF);
                    key.extend_from_slice(event_type);

                    db.roomusertype_roomuserdataid
                        .insert(&key, &roomuserdataid)?;
                }

                services().globals.bump_database_version(5)?;

                warn!("Migration: 4 -> 5 finished");
            }

            if services().globals.database_version()? < 6 {
                // Set room member count
                for (roomid, _) in db.roomid_shortstatehash.iter() {
                    let string = utils::string_from_bytes(&roomid).unwrap();
                    let room_id = <&RoomId>::try_from(string.as_str()).unwrap();
                    services()
                        .rooms
                        .state_cache
                        .update_joined_count(room_id)?;
                }

                services().globals.bump_database_version(6)?;

                warn!("Migration: 5 -> 6 finished");
            }

            if services().globals.database_version()? < 7 {
                // Upgrade state store
                let mut last_roomstates: HashMap<OwnedRoomId, u64> =
                    HashMap::new();
                let mut current_sstatehash: Option<u64> = None;
                let mut current_room = None;
                let mut current_state = HashSet::new();
                let mut counter = 0;

                let mut handle_state =
                    |current_sstatehash: u64,
                     current_room: &RoomId,
                     current_state: HashSet<_>,
                     last_roomstates: &mut HashMap<_, _>| {
                        counter += 1;
                        let last_roomsstatehash =
                            last_roomstates.get(current_room);

                        let states_parents = last_roomsstatehash.map_or_else(
                            || Ok(Vec::new()),
                            |&last_roomsstatehash| {
                                services()
                                    .rooms
                                    .state_compressor
                                    .load_shortstatehash_info(
                                        last_roomsstatehash,
                                    )
                            },
                        )?;

                        let (statediffnew, statediffremoved) =
                            if let Some(parent_stateinfo) =
                                states_parents.last()
                            {
                                let statediffnew = current_state
                                    .difference(&parent_stateinfo.full_state)
                                    .copied()
                                    .collect::<HashSet<_>>();

                                let statediffremoved = parent_stateinfo
                                    .full_state
                                    .difference(&current_state)
                                    .copied()
                                    .collect::<HashSet<_>>();

                                (statediffnew, statediffremoved)
                            } else {
                                (current_state, HashSet::new())
                            };

                        services()
                            .rooms
                            .state_compressor
                            .save_state_from_diff(
                                current_sstatehash,
                                Arc::new(statediffnew),
                                Arc::new(statediffremoved),
                                // every state change is 2 event changes on
                                // average
                                2,
                                states_parents,
                            )?;

                        Ok::<_, Error>(())
                    };

                for (k, seventid) in
                    db.db.open_tree("stateid_shorteventid")?.iter()
                {
                    let sstatehash =
                        utils::u64_from_bytes(&k[0..size_of::<u64>()])
                            .expect("number of bytes is correct");
                    let sstatekey = k[size_of::<u64>()..].to_vec();
                    if Some(sstatehash) != current_sstatehash {
                        if let Some(current_sstatehash) = current_sstatehash {
                            handle_state(
                                current_sstatehash,
                                current_room.as_deref().unwrap(),
                                current_state,
                                &mut last_roomstates,
                            )?;
                            last_roomstates.insert(
                                current_room.clone().unwrap(),
                                current_sstatehash,
                            );
                        }
                        current_state = HashSet::new();
                        current_sstatehash = Some(sstatehash);

                        let event_id = db
                            .shorteventid_eventid
                            .get(&seventid)
                            .unwrap()
                            .unwrap();
                        let string =
                            utils::string_from_bytes(&event_id).unwrap();
                        let event_id =
                            <&EventId>::try_from(string.as_str()).unwrap();
                        let pdu = services()
                            .rooms
                            .timeline
                            .get_pdu(event_id)
                            .unwrap()
                            .unwrap();

                        if Some(&pdu.room_id) != current_room.as_ref() {
                            current_room = Some(pdu.room_id.clone());
                        }
                    }

                    let mut val = sstatekey;
                    val.extend_from_slice(&seventid);
                    current_state
                        .insert(val.try_into().expect("size is correct"));
                }

                if let Some(current_sstatehash) = current_sstatehash {
                    handle_state(
                        current_sstatehash,
                        current_room.as_deref().unwrap(),
                        current_state,
                        &mut last_roomstates,
                    )?;
                }

                services().globals.bump_database_version(7)?;

                warn!("Migration: 6 -> 7 finished");
            }

            if services().globals.database_version()? < 8 {
                // Generate short room ids for all rooms
                for (room_id, _) in db.roomid_shortstatehash.iter() {
                    let shortroomid =
                        services().globals.next_count()?.to_be_bytes();
                    db.roomid_shortroomid.insert(&room_id, &shortroomid)?;
                    info!("Migration: 8");
                }
                // Update pduids db layout
                let mut batch = db.pduid_pdu.iter().filter_map(|(key, v)| {
                    if !key.starts_with(b"!") {
                        return None;
                    }
                    let mut parts = key.splitn(2, |&b| b == 0xFF);
                    let room_id = parts.next().unwrap();
                    let count = parts.next().unwrap();

                    let short_room_id = db
                        .roomid_shortroomid
                        .get(room_id)
                        .unwrap()
                        .expect("shortroomid should exist");

                    let mut new_key = short_room_id;
                    new_key.extend_from_slice(count);

                    Some((new_key, v))
                });

                db.pduid_pdu.insert_batch(&mut batch)?;

                let mut batch2 =
                    db.eventid_pduid.iter().filter_map(|(k, value)| {
                        if !value.starts_with(b"!") {
                            return None;
                        }
                        let mut parts = value.splitn(2, |&b| b == 0xFF);
                        let room_id = parts.next().unwrap();
                        let count = parts.next().unwrap();

                        let short_room_id = db
                            .roomid_shortroomid
                            .get(room_id)
                            .unwrap()
                            .expect("shortroomid should exist");

                        let mut new_value = short_room_id;
                        new_value.extend_from_slice(count);

                        Some((k, new_value))
                    });

                db.eventid_pduid.insert_batch(&mut batch2)?;

                services().globals.bump_database_version(8)?;

                warn!("Migration: 7 -> 8 finished");
            }

            if services().globals.database_version()? < 9 {
                // Update tokenids db layout
                let mut iter = db
                    .tokenids
                    .iter()
                    .filter_map(|(key, _)| {
                        if !key.starts_with(b"!") {
                            return None;
                        }
                        let mut parts = key.splitn(4, |&b| b == 0xFF);
                        let room_id = parts.next().unwrap();
                        let word = parts.next().unwrap();
                        let _pdu_id_room = parts.next().unwrap();
                        let pdu_id_count = parts.next().unwrap();

                        let short_room_id = db
                            .roomid_shortroomid
                            .get(room_id)
                            .unwrap()
                            .expect("shortroomid should exist");
                        let mut new_key = short_room_id;
                        new_key.extend_from_slice(word);
                        new_key.push(0xFF);
                        new_key.extend_from_slice(pdu_id_count);
                        Some((new_key, Vec::new()))
                    })
                    .peekable();

                while iter.peek().is_some() {
                    db.tokenids.insert_batch(&mut iter.by_ref().take(1000))?;
                    debug!("Inserted smaller batch");
                }

                info!("Deleting starts");

                let batch2: Vec<_> = db
                    .tokenids
                    .iter()
                    .filter_map(|(key, _)| key.starts_with(b"!").then_some(key))
                    .collect();

                for key in batch2 {
                    db.tokenids.remove(&key)?;
                }

                services().globals.bump_database_version(9)?;

                warn!("Migration: 8 -> 9 finished");
            }

            if services().globals.database_version()? < 10 {
                // Add other direction for shortstatekeys
                for (statekey, shortstatekey) in
                    db.statekey_shortstatekey.iter()
                {
                    db.shortstatekey_statekey
                        .insert(&shortstatekey, &statekey)?;
                }

                // Force E2EE device list updates so we can send them over
                // federation
                for user_id in services().users.iter().filter_map(Result::ok) {
                    services().users.mark_device_key_update(&user_id)?;
                }

                services().globals.bump_database_version(10)?;

                warn!("Migration: 9 -> 10 finished");
            }

            if services().globals.database_version()? < 11 {
                db.db.open_tree("userdevicesessionid_uiaarequest")?.clear()?;
                services().globals.bump_database_version(11)?;

                warn!("Migration: 10 -> 11 finished");
            }

            if services().globals.database_version()? < 12 {
                for username in services().users.list_local_users()? {
                    let user = match UserId::parse_with_server_name(
                        username.clone(),
                        services().globals.server_name(),
                    ) {
                        Ok(u) => u,
                        Err(error) => {
                            warn!(
                                %error,
                                user_localpart = %username,
                                "Invalid username",
                            );
                            continue;
                        }
                    };

                    let raw_rules_list = services()
                        .account_data
                        .get(
                            None,
                            &user,
                            GlobalAccountDataEventType::PushRules
                                .to_string()
                                .into(),
                        )
                        .unwrap()
                        .expect("Username is invalid");

                    let mut account_data =
                        serde_json::from_str::<PushRulesEvent>(
                            raw_rules_list.get(),
                        )
                        .unwrap();
                    let rules_list = &mut account_data.content.global;

                    //content rule
                    {
                        let content_rule_transformation = [
                            ".m.rules.contains_user_name",
                            ".m.rule.contains_user_name",
                        ];

                        let rule = rules_list
                            .content
                            .get(content_rule_transformation[0]);
                        if rule.is_some() {
                            let mut rule = rule.unwrap().clone();
                            content_rule_transformation[1]
                                .clone_into(&mut rule.rule_id);
                            rules_list
                                .content
                                .shift_remove(content_rule_transformation[0]);
                            rules_list.content.insert(rule);
                        }
                    }

                    //underride rules
                    {
                        let underride_rule_transformation = [
                            [".m.rules.call", ".m.rule.call"],
                            [
                                ".m.rules.room_one_to_one",
                                ".m.rule.room_one_to_one",
                            ],
                            [
                                ".m.rules.encrypted_room_one_to_one",
                                ".m.rule.encrypted_room_one_to_one",
                            ],
                            [".m.rules.message", ".m.rule.message"],
                            [".m.rules.encrypted", ".m.rule.encrypted"],
                        ];

                        for transformation in underride_rule_transformation {
                            let rule =
                                rules_list.underride.get(transformation[0]);
                            if let Some(rule) = rule {
                                let mut rule = rule.clone();
                                transformation[1].clone_into(&mut rule.rule_id);
                                rules_list
                                    .underride
                                    .shift_remove(transformation[0]);
                                rules_list.underride.insert(rule);
                            }
                        }
                    }

                    services().account_data.update(
                        None,
                        &user,
                        GlobalAccountDataEventType::PushRules
                            .to_string()
                            .into(),
                        &serde_json::to_value(account_data)
                            .expect("to json value always works"),
                    )?;
                }

                services().globals.bump_database_version(12)?;

                warn!("Migration: 11 -> 12 finished");
            }

            // This migration can be reused as-is anytime the server-default
            // rules are updated.
            if services().globals.database_version()? < 13 {
                for username in services().users.list_local_users()? {
                    let user = match UserId::parse_with_server_name(
                        username.clone(),
                        services().globals.server_name(),
                    ) {
                        Ok(u) => u,
                        Err(error) => {
                            warn!(
                                %error,
                                user_localpart = %username,
                                "Invalid username",
                            );
                            continue;
                        }
                    };

                    let raw_rules_list = services()
                        .account_data
                        .get(
                            None,
                            &user,
                            GlobalAccountDataEventType::PushRules
                                .to_string()
                                .into(),
                        )
                        .unwrap()
                        .expect("Username is invalid");

                    let mut account_data =
                        serde_json::from_str::<PushRulesEvent>(
                            raw_rules_list.get(),
                        )
                        .unwrap();

                    let user_default_rules = Ruleset::server_default(&user);
                    account_data
                        .content
                        .global
                        .update_with_server_default(user_default_rules);

                    services().account_data.update(
                        None,
                        &user,
                        GlobalAccountDataEventType::PushRules
                            .to_string()
                            .into(),
                        &serde_json::to_value(account_data)
                            .expect("to json value always works"),
                    )?;
                }

                services().globals.bump_database_version(13)?;

                warn!("Migration: 12 -> 13 finished");
            }

            assert_eq!(
                services().globals.database_version().unwrap(),
                latest_database_version,
                "database should be migrated to the current version",
            );

            info!(
                backend = %services().globals.config.database.backend,
                version = latest_database_version,
                "Loaded database",
            );
        } else {
            services()
                .globals
                .bump_database_version(latest_database_version)?;

            // Create the admin room and server user on first run
            services().admin.create_admin_room().await?;

            info!(
                backend = %services().globals.config.database.backend,
                version = latest_database_version,
                "Created new database",
            );
        }

        services().admin.start_handler();

        // Set emergency access for the grapevine user
        match set_emergency_access() {
            Ok(pwd_set) => {
                if pwd_set {
                    warn!(
                        "The Grapevine account emergency password is set! \
                         Please unset it as soon as you finish admin account \
                         recovery!"
                    );
                    services().admin.send_message(
                        RoomMessageEventContent::text_plain(
                            "The Grapevine account emergency password is set! \
                             Please unset it as soon as you finish admin \
                             account recovery!",
                        ),
                    );
                }
            }
            Err(error) => {
                error!(
                    %error,
                    "Could not set the configured emergency password for the \
                     Grapevine user",
                );
            }
        };

        services().sending.start_handler();

        Self::start_cleanup_task();

        Ok(())
    }

    #[tracing::instrument]
    pub(crate) fn start_cleanup_task() {
        use std::time::{Duration, Instant};

        #[cfg(unix)]
        use tokio::signal::unix::{signal, SignalKind};
        use tokio::time::interval;

        let timer_interval = Duration::from_secs(u64::from(
            services().globals.config.cleanup_second_interval,
        ));

        tokio::spawn(async move {
            let mut i = interval(timer_interval);
            #[cfg(unix)]
            let mut s = signal(SignalKind::hangup()).unwrap();

            loop {
                #[cfg(unix)]
                let msg = tokio::select! {
                    _ = i.tick() => || {
                        debug!("cleanup: Timer ticked");
                    },
                    _ = s.recv() => || {
                        debug!("cleanup: Received SIGHUP");
                    },
                };
                #[cfg(not(unix))]
                let msg = {
                    i.tick().await;
                    || debug!("cleanup: Timer ticked")
                };

                async {
                    msg();
                    let start = Instant::now();
                    if let Err(error) = services().globals.cleanup() {
                        error!(%error, "cleanup: Error");
                    } else {
                        debug!(elapsed = ?start.elapsed(), "cleanup: Finished");
                    }
                }
                .instrument(info_span!("database_cleanup"))
                .await;
            }
        });
    }
}

/// Sets the emergency password and push rules for the @grapevine account in
/// case emergency password is set
fn set_emergency_access() -> Result<bool> {
    let admin_bot = services().globals.admin_bot_user_id.as_ref();

    services().users.set_password(
        admin_bot,
        services().globals.emergency_password().as_deref(),
    )?;

    let (ruleset, res) = match services().globals.emergency_password() {
        Some(_) => (Ruleset::server_default(admin_bot), Ok(true)),
        None => (Ruleset::new(), Ok(false)),
    };

    services().account_data.update(
        None,
        admin_bot,
        GlobalAccountDataEventType::PushRules.to_string().into(),
        &serde_json::to_value(&GlobalAccountDataEvent {
            content: PushRulesEventContent {
                global: ruleset,
            },
        })
        .expect("to json value always works"),
    )?;

    res
}
