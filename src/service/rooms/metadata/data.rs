use ruma::{OwnedRoomId, RoomId};

use crate::Result;

pub(crate) trait Data: Send + Sync {
    /// Checks if a room exists.
    fn exists(&self, room_id: &RoomId) -> Result<bool>;
    fn iter_ids<'a>(
        &'a self,
    ) -> Box<dyn Iterator<Item = Result<OwnedRoomId>> + 'a>;
    fn is_disabled(&self, room_id: &RoomId) -> Result<bool>;
    fn disable_room(&self, room_id: &RoomId, disabled: bool) -> Result<()>;
}
