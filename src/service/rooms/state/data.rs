use std::{collections::HashSet, sync::Arc};

use ruma::{EventId, OwnedEventId, RoomId};
use tokio::sync::MutexGuard;

use crate::Result;

pub(crate) trait Data: Send + Sync {
    /// Returns the last state hash key added to the db for the given room.
    fn get_room_shortstatehash(&self, room_id: &RoomId) -> Result<Option<u64>>;

    /// Set the state hash to a new version, but does not update `state_cache`.
    fn set_room_state(
        &self,
        room_id: &RoomId,
        new_shortstatehash: u64,
        // Take mutex guard to make sure users get the room state mutex
        _mutex_lock: &MutexGuard<'_, ()>,
    ) -> Result<()>;

    /// Associates a state with an event.
    fn set_event_state(
        &self,
        shorteventid: u64,
        shortstatehash: u64,
    ) -> Result<()>;

    /// Returns all events we would send as the `prev_events` of the next event.
    fn get_forward_extremities(
        &self,
        room_id: &RoomId,
    ) -> Result<HashSet<Arc<EventId>>>;

    /// Replace the forward extremities of the room.
    fn set_forward_extremities(
        &self,
        room_id: &RoomId,
        event_ids: Vec<OwnedEventId>,
        // Take mutex guard to make sure users get the room state mutex
        _mutex_lock: &MutexGuard<'_, ()>,
    ) -> Result<()>;
}
