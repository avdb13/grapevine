use std::{collections::HashSet, sync::Arc};

use ruma::{EventId, OwnedEventId, OwnedRoomId, RoomId};

use crate::{
    service::globals::marker, utils::on_demand_hashmap::KeyToken, Result,
};

pub(crate) trait Data: Send + Sync {
    /// Returns the last state hash key added to the db for the given room.
    fn get_room_shortstatehash(&self, room_id: &RoomId) -> Result<Option<u64>>;

    /// Set the state hash to a new version, but does not update `state_cache`.
    fn set_room_state(
        &self,
        room_id: &KeyToken<OwnedRoomId, marker::State>,
        new_shortstatehash: u64,
    ) -> Result<()>;

    fn remove_room_state(
        &self,
        room_id: &KeyToken<OwnedRoomId, marker::State>,
    ) -> Result<()>;

    /// Associates a state with an event.
    fn set_event_state(
        &self,
        shorteventid: u64,
        shortstatehash: u64,
    ) -> Result<()>;

    fn remove_event_state(&self, shorteventid: u64) -> Result<()>;

    /// Returns all events we would send as the `prev_events` of the next event.
    fn get_forward_extremities(
        &self,
        room_id: &RoomId,
    ) -> Result<HashSet<Arc<EventId>>>;

    /// Replace the forward extremities of the room.
    fn set_forward_extremities(
        &self,
        room_id: &KeyToken<OwnedRoomId, marker::State>,
        event_ids: Vec<OwnedEventId>,
    ) -> Result<()>;
}
