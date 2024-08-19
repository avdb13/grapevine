use ruma::{OwnedRoomId, RoomId};

use super::globals::marker;
use crate::{utils::on_demand_hashmap::KeyToken, Result};

pub(crate) mod alias;
pub(crate) mod auth_chain;
pub(crate) mod directory;
pub(crate) mod edus;
pub(crate) mod event_handler;
pub(crate) mod lazy_loading;
pub(crate) mod metadata;
pub(crate) mod outlier;
pub(crate) mod pdu_metadata;
pub(crate) mod search;
pub(crate) mod short;
pub(crate) mod spaces;
pub(crate) mod state;
pub(crate) mod state_accessor;
pub(crate) mod state_cache;
pub(crate) mod state_compressor;
pub(crate) mod threads;
pub(crate) mod timeline;
pub(crate) mod user;

pub(crate) trait Data:
    Send
    + Sync
    + alias::Data
    + auth_chain::Data
    + directory::Data
    + edus::Data
    + lazy_loading::Data
    + metadata::Data
    + outlier::Data
    + pdu_metadata::Data
    + search::Data
    + short::Data
    + state::Data
    + state_accessor::Data
    + state_cache::Data
    + state_compressor::Data
    + timeline::Data
    + threads::Data
    + user::Data
{
    #[allow(dead_code)]
    fn purge(
        &self,
        room_id: &RoomId,
        room_token: &KeyToken<OwnedRoomId, marker::State>,
    ) -> Result<()>;
}

pub(crate) struct Service {
    pub(crate) alias: alias::Service,
    pub(crate) auth_chain: auth_chain::Service,
    pub(crate) directory: directory::Service,
    pub(crate) edus: edus::Service,
    pub(crate) event_handler: event_handler::Service,
    pub(crate) lazy_loading: lazy_loading::Service,
    pub(crate) metadata: metadata::Service,
    pub(crate) outlier: outlier::Service,
    pub(crate) pdu_metadata: pdu_metadata::Service,
    pub(crate) search: search::Service,
    pub(crate) short: short::Service,
    pub(crate) state: state::Service,
    pub(crate) state_accessor: state_accessor::Service,
    pub(crate) state_cache: state_cache::Service,
    pub(crate) state_compressor: state_compressor::Service,
    pub(crate) timeline: timeline::Service,
    pub(crate) threads: threads::Service,
    pub(crate) spaces: spaces::Service,
    pub(crate) user: user::Service,
}
