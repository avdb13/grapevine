use std::sync::Arc;

use ruma::{events::StateEventType, EventId, RoomId};

use crate::{
    database::KeyValueDatabase,
    observability::{FoundIn, Lookup, METRICS},
    service, services, utils, Error, Result,
};

impl service::rooms::short::Data for KeyValueDatabase {
    #[tracing::instrument(skip(self))]
    fn get_or_create_shorteventid(&self, event_id: &EventId) -> Result<u64> {
        let lookup = Lookup::CreateEventIdToShort;

        if let Some(short) =
            self.eventidshort_cache.lock().unwrap().get_mut(event_id)
        {
            METRICS.record_lookup(lookup, FoundIn::Cache);
            return Ok(*short);
        }

        let short = if let Some(shorteventid) =
            self.eventid_shorteventid.get(event_id.as_bytes())?
        {
            METRICS.record_lookup(lookup, FoundIn::Database);

            utils::u64_from_bytes(&shorteventid).map_err(|_| {
                Error::bad_database("Invalid shorteventid in db.")
            })?
        } else {
            METRICS.record_lookup(lookup, FoundIn::Nothing);

            let shorteventid = services().globals.next_count()?;
            self.eventid_shorteventid
                .insert(event_id.as_bytes(), &shorteventid.to_be_bytes())?;
            self.shorteventid_eventid
                .insert(&shorteventid.to_be_bytes(), event_id.as_bytes())?;
            shorteventid
        };

        self.eventidshort_cache
            .lock()
            .unwrap()
            .insert(event_id.to_owned(), short);

        Ok(short)
    }

    #[tracing::instrument(skip(self), fields(cache_result))]
    fn get_shortstatekey(
        &self,
        event_type: &StateEventType,
        state_key: &str,
    ) -> Result<Option<u64>> {
        let lookup = Lookup::StateKeyToShort;

        if let Some(short) = self
            .statekeyshort_cache
            .lock()
            .unwrap()
            .get_mut(&(event_type.clone(), state_key.to_owned()))
        {
            METRICS.record_lookup(lookup, FoundIn::Cache);
            return Ok(Some(*short));
        }

        let mut db_key = event_type.to_string().as_bytes().to_vec();
        db_key.push(0xFF);
        db_key.extend_from_slice(state_key.as_bytes());

        let short = self
            .statekey_shortstatekey
            .get(&db_key)?
            .map(|shortstatekey| {
                utils::u64_from_bytes(&shortstatekey).map_err(|_| {
                    Error::bad_database("Invalid shortstatekey in db.")
                })
            })
            .transpose()?;

        if let Some(s) = short {
            METRICS.record_lookup(lookup, FoundIn::Database);

            self.statekeyshort_cache
                .lock()
                .unwrap()
                .insert((event_type.clone(), state_key.to_owned()), s);
        } else {
            METRICS.record_lookup(lookup, FoundIn::Nothing);
        }

        Ok(short)
    }

    #[tracing::instrument(skip(self))]
    fn get_or_create_shortstatekey(
        &self,
        event_type: &StateEventType,
        state_key: &str,
    ) -> Result<u64> {
        let lookup = Lookup::CreateStateKeyToShort;

        if let Some(short) = self
            .statekeyshort_cache
            .lock()
            .unwrap()
            .get_mut(&(event_type.clone(), state_key.to_owned()))
        {
            METRICS.record_lookup(lookup, FoundIn::Cache);
            return Ok(*short);
        }

        let mut db_key = event_type.to_string().as_bytes().to_vec();
        db_key.push(0xFF);
        db_key.extend_from_slice(state_key.as_bytes());

        let short = if let Some(shortstatekey) =
            self.statekey_shortstatekey.get(&db_key)?
        {
            METRICS.record_lookup(lookup, FoundIn::Database);

            utils::u64_from_bytes(&shortstatekey).map_err(|_| {
                Error::bad_database("Invalid shortstatekey in db.")
            })?
        } else {
            METRICS.record_lookup(lookup, FoundIn::Nothing);

            let shortstatekey = services().globals.next_count()?;
            self.statekey_shortstatekey
                .insert(&db_key, &shortstatekey.to_be_bytes())?;
            self.shortstatekey_statekey
                .insert(&shortstatekey.to_be_bytes(), &db_key)?;
            shortstatekey
        };

        self.statekeyshort_cache
            .lock()
            .unwrap()
            .insert((event_type.clone(), state_key.to_owned()), short);

        Ok(short)
    }

    #[tracing::instrument(skip(self))]
    fn get_eventid_from_short(
        &self,
        shorteventid: u64,
    ) -> Result<Arc<EventId>> {
        let lookup = Lookup::ShortToEventId;

        if let Some(id) =
            self.shorteventid_cache.lock().unwrap().get_mut(&shorteventid)
        {
            METRICS.record_lookup(lookup, FoundIn::Cache);
            return Ok(Arc::clone(id));
        }

        let bytes = self
            .shorteventid_eventid
            .get(&shorteventid.to_be_bytes())?
            .ok_or_else(|| {
                Error::bad_database("Shorteventid does not exist")
            })?;

        let event_id = EventId::parse_arc(
            utils::string_from_bytes(&bytes).map_err(|_| {
                Error::bad_database(
                    "EventID in shorteventid_eventid is invalid unicode.",
                )
            })?,
        )
        .map_err(|_| {
            Error::bad_database("EventId in shorteventid_eventid is invalid.")
        })?;

        METRICS.record_lookup(lookup, FoundIn::Database);

        self.shorteventid_cache
            .lock()
            .unwrap()
            .insert(shorteventid, Arc::clone(&event_id));

        Ok(event_id)
    }

    #[tracing::instrument(skip(self))]
    fn get_statekey_from_short(
        &self,
        shortstatekey: u64,
    ) -> Result<(StateEventType, String)> {
        let lookup = Lookup::ShortToStateKey;

        if let Some(id) =
            self.shortstatekey_cache.lock().unwrap().get_mut(&shortstatekey)
        {
            METRICS.record_lookup(lookup, FoundIn::Cache);
            return Ok(id.clone());
        }

        let bytes = self
            .shortstatekey_statekey
            .get(&shortstatekey.to_be_bytes())?
            .ok_or_else(|| {
                Error::bad_database("Shortstatekey does not exist")
            })?;

        let mut parts = bytes.splitn(2, |&b| b == 0xFF);
        let eventtype_bytes =
            parts.next().expect("split always returns one entry");
        let statekey_bytes = parts.next().ok_or_else(|| {
            Error::bad_database("Invalid statekey in shortstatekey_statekey.")
        })?;

        let event_type = StateEventType::from(
            utils::string_from_bytes(eventtype_bytes).map_err(|_| {
                Error::bad_database(
                    "Event type in shortstatekey_statekey is invalid unicode.",
                )
            })?,
        );

        let state_key =
            utils::string_from_bytes(statekey_bytes).map_err(|_| {
                Error::bad_database(
                    "Statekey in shortstatekey_statekey is invalid unicode.",
                )
            })?;

        let result = (event_type, state_key);

        METRICS.record_lookup(lookup, FoundIn::Database);

        self.shortstatekey_cache
            .lock()
            .unwrap()
            .insert(shortstatekey, result.clone());

        Ok(result)
    }

    /// Returns `(shortstatehash, already_existed)`
    #[tracing::instrument(skip(self))]
    fn get_or_create_shortstatehash(
        &self,
        state_hash: &[u8],
    ) -> Result<(u64, bool)> {
        Ok(
            if let Some(shortstatehash) =
                self.statehash_shortstatehash.get(state_hash)?
            {
                (
                    utils::u64_from_bytes(&shortstatehash).map_err(|_| {
                        Error::bad_database("Invalid shortstatehash in db.")
                    })?,
                    true,
                )
            } else {
                let shortstatehash = services().globals.next_count()?;
                self.statehash_shortstatehash
                    .insert(state_hash, &shortstatehash.to_be_bytes())?;
                (shortstatehash, false)
            },
        )
    }

    fn get_shortroomid(&self, room_id: &RoomId) -> Result<Option<u64>> {
        self.roomid_shortroomid
            .get(room_id.as_bytes())?
            .map(|bytes| {
                utils::u64_from_bytes(&bytes).map_err(|_| {
                    Error::bad_database("Invalid shortroomid in db.")
                })
            })
            .transpose()
    }

    fn remove_shortroomid(&self, room_id: &RoomId) -> Result<()> {
        self.roomid_shortroomid.remove(room_id.as_bytes())
    }

    fn get_or_create_shortroomid(&self, room_id: &RoomId) -> Result<u64> {
        Ok(
            if let Some(short) =
                self.roomid_shortroomid.get(room_id.as_bytes())?
            {
                utils::u64_from_bytes(&short).map_err(|_| {
                    Error::bad_database("Invalid shortroomid in db.")
                })?
            } else {
                let short = services().globals.next_count()?;
                self.roomid_shortroomid
                    .insert(room_id.as_bytes(), &short.to_be_bytes())?;
                short
            },
        )
    }
}
