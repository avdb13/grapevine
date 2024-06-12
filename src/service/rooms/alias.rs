use ruma::{OwnedRoomAliasId, OwnedRoomId, RoomAliasId, RoomId};

use crate::Result;

mod data;

pub(crate) use data::Data;

pub(crate) struct Service {
    db: &'static dyn Data,
}

impl Service {
    pub(crate) fn new<D>(db: &'static D) -> Self
    where
        D: Data,
    {
        Self {
            db,
        }
    }

    /// Creates or updates the alias to the given room id.
    pub(crate) fn set_alias(
        &self,
        alias: &RoomAliasId,
        room_id: &RoomId,
    ) -> Result<()> {
        self.db.set_alias(alias, room_id)
    }

    /// Forgets about an alias. Returns an error if the alias did not exist.
    pub(crate) fn remove_alias(&self, alias: &RoomAliasId) -> Result<()> {
        self.db.remove_alias(alias)
    }

    /// Looks up the roomid for the given alias.
    pub(crate) fn resolve_local_alias(
        &self,
        alias: &RoomAliasId,
    ) -> Result<Option<OwnedRoomId>> {
        self.db.resolve_local_alias(alias)
    }

    /// Returns all local aliases that point to the given room
    pub(crate) fn local_aliases_for_room<'a>(
        &'a self,
        room_id: &RoomId,
    ) -> Box<dyn Iterator<Item = Result<OwnedRoomAliasId>> + 'a> {
        self.db.local_aliases_for_room(room_id)
    }
}
