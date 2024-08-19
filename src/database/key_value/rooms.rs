mod alias;
mod auth_chain;
mod directory;
mod edus;
mod lazy_load;
mod metadata;
mod outlier;
mod pdu_metadata;
mod search;
mod short;
mod state;
mod state_accessor;
mod state_cache;
mod state_compressor;
mod threads;
mod timeline;
mod user;

use std::collections::HashSet;

use ruma::{
    api::client::threads::get_threads::v1::IncludeThreads, OwnedRoomId, RoomId,
    UserId,
};

use crate::{
    database::KeyValueDatabase,
    service::{
        self,
        globals::marker,
        rooms::{
            alias::Data as _, auth_chain::Data as _, directory::Data as _,
            edus::read_receipt::Data as _, metadata::Data as _,
            outlier::Data as _, search::Data as _, short::Data as _,
            state::Data as _, state_cache::Data as _,
            state_compressor::Data as _, threads::Data, timeline::Data as _,
        },
        users::Data as _,
    },
    services,
    utils::{self, on_demand_hashmap::KeyToken},
    Error, PduEvent, Result,
};

impl service::rooms::Data for KeyValueDatabase {
    #[allow(clippy::too_many_lines)]
    fn purge(
        &self,
        room_id: &RoomId,
        room_token: &KeyToken<OwnedRoomId, marker::State>,
    ) -> Result<()> {
        let shortroomid = self.get_shortroomid(room_id)?.ok_or_else(|| {
            Error::bad_database("Looked for bad shortroomid in timeline")
        })?;

        let room_members: Vec<_> =
            self.room_members(room_id).collect::<Result<_>>()?;

        // TODO: return errors?
        let pdus: Vec<_> = self
            .pduid_pdu
            .scan_prefix(room_id.as_bytes().to_vec())
            .filter_map(|(_, v)| serde_json::from_slice::<PduEvent>(&v).ok())
            .collect();

        let aliases: Vec<_> =
            self.local_aliases_for_room(room_id).collect::<Result<_>>()?;
        // alias
        for alias in aliases {
            self.remove_alias(&alias)?;
        }

        // auth_chain
        for event_id in pdus.iter().map(|pdu| &*pdu.event_id) {
            if let Some(shorteventid) = self.get_shorteventid(event_id)? {
                self.remove_cached_eventid_authchain(&[shorteventid])?;
            }
        }

        // directory
        self.set_not_public(room_id)?;

        // edus
        self.readreceipts_reset(room_id)?;
        self.private_read_reset(room_id)?;
        self.last_privateread_reset(room_id)?;

        // lazy_load
        for user_id in &room_members {
            let devices: Vec<_> =
                self.all_device_ids(user_id).collect::<Result<_>>()?;

            for device_id in devices {
                let mut prefix = user_id.as_bytes().to_vec();
                prefix.push(0xFF);
                prefix.extend_from_slice(device_id.as_bytes());
                prefix.push(0xFF);
                prefix.extend_from_slice(room_id.as_bytes());
                prefix.push(0xFF);

                for (key, _) in self.lazyloadedids.scan_prefix(prefix) {
                    self.lazyloadedids.remove(&key)?;
                }
            }
        }

        // metadata
        self.disable_room(room_id, false)?;

        // outlier
        for event_id in pdus.iter().map(|pdu| &*pdu.event_id) {
            self.remove_pdu_outlier(event_id)?;
        }

        // TODO: pdu_metadata

        // search
        for (event_id, content) in
            pdus.iter().map(|pdu| (&*pdu.event_id, pdu.content.get()))
        {
            #[derive(serde::Deserialize)]
            struct ExtractBody {
                body: String,
            }

            let content: Option<ExtractBody> =
                serde_json::from_str(content).ok();

            if let Some((pdu_id, content)) =
                self.get_pdu_id(event_id).map(|pdu_id| pdu_id.zip(content))?
            {
                self.deindex_pdu(shortroomid, &pdu_id, &content.body)?;
            }
        }

        // short
        self.remove_shortroomid(room_id)?;

        for event_id in pdus.iter().map(|pdu| &*pdu.event_id) {
            self.remove_shorteventid(event_id)?;
        }

        // state
        self.remove_room_state(room_token)?;
        self.set_forward_extremities(room_token, Vec::default())?;

        for event_id in pdus.iter().map(|pdu| &*pdu.event_id) {
            if let Some(shorteventid) = self.get_shorteventid(event_id)? {
                self.remove_event_state(shorteventid)?;
            }
        }

        // TODO: state_accessor

        // timeline
        {
            let shortroomid =
                self.get_shortroomid(room_id)?.map(u64::to_be_bytes);

            let Some(prefix) = shortroomid.as_ref().map(Vec::from) else {
                return Err(Error::bad_database(
                    "Looked for bad shortroomid in timeline",
                ));
            };

            // PduId = ShortRoomId + Count
            for (key, value) in self.pduid_pdu.scan_prefix(prefix) {
                self.pduid_pdu.remove(&key)?;

                let PduEvent {
                    event_id,
                    ..
                } = serde_json::from_slice(&value).map_err(|_| {
                    Error::bad_database("PDU in db is invalid.")
                })?;

                self.eventid_pduid.remove(event_id.as_bytes())?;
                self.eventid_outlierpdu.remove(event_id.as_bytes())?;

                let mut cache = self.eventidshort_cache.lock().unwrap();
                cache.remove(&*event_id);

                let value =
                    self.eventid_shorteventid.get(event_id.as_bytes())?;
                if let Some(shorteventid) = value
                    .map(|value| {
                        utils::u64_from_bytes(&value).map_err(|_| {
                            Error::bad_database("Invalid shortstatekey in db.")
                        })
                    })
                    .transpose()?
                {
                    self.shorteventid_shortstatehash
                        .remove(&shorteventid.to_be_bytes())?;

                    self.remove_cached_eventid_authchain(&[shorteventid])?;
                }
            }
        }

        // state
        self.roomid_shortstatehash.remove(room_id.as_bytes())?;

        let mut prefix = room_id.as_bytes().to_vec();
        prefix.push(0xFF);

        for (key, _) in self.roomid_pduleaves.scan_prefix(prefix.clone()) {
            self.roomid_pduleaves.remove(&key)?;
        }

        // TODO: state_accessor

        // state_cache
        let joined: HashSet<_> =
            self.room_members(room_id).collect::<Result<_>>()?;
        let invited: HashSet<_> =
            self.room_members_invited(room_id).collect::<Result<_>>()?;

        for user_id in joined.iter().chain(&invited) {
            self.clear_markers(user_id, room_id)?;
        }
        self.update_joined_count(room_id)?;

        self.roomid_joinedcount.remove(room_id.as_bytes())?;
        self.roomid_invitedcount.remove(room_id.as_bytes())?;

        self.our_real_users_cache.write().unwrap().remove(room_id);

        // state_compressor
        if let Some(shortstatehash) =
            services().rooms.state.get_room_shortstatehash(room_id)?
        {
            self.remove_statediff(shortstatehash)?;

            services()
                .rooms
                .state_compressor
                .stateinfo_cache
                .lock()
                .unwrap()
                .remove(&shortstatehash);
        }

        // threads
        let user_id = UserId::parse_with_server_name(
            "",
            services().globals.server_name(),
        )
        .expect("we know this is valid");

        let threads: Vec<_> = self
            .threads_until(&user_id, room_id, u64::MAX, &IncludeThreads::All)
            .and_then(Iterator::collect::<Result<_>>)?;

        for (pdu_id, _) in threads {
            self.reset_participants(&pdu_id.to_be_bytes())?;
        }

        // timeline

        Ok(())
    }
}
