use ruma::{OwnedRoomId, RoomId};

use crate::{database::KeyValueDatabase, service, utils, Error, Result};

impl service::rooms::directory::Data for KeyValueDatabase {
    #[tracing::instrument(skip(self))]
    fn set_public(&self, room_id: &RoomId) -> Result<()> {
        self.publicroomids.insert(room_id.as_bytes(), &[])
    }

    #[tracing::instrument(skip(self))]
    fn set_not_public(&self, room_id: &RoomId) -> Result<()> {
        self.publicroomids.remove(room_id.as_bytes())
    }

    #[tracing::instrument(skip(self))]
    fn is_public_room(&self, room_id: &RoomId) -> Result<bool> {
        Ok(self.publicroomids.get(room_id.as_bytes())?.is_some())
    }

    #[tracing::instrument(skip(self))]
    fn public_rooms<'a>(
        &'a self,
    ) -> Box<dyn Iterator<Item = Result<OwnedRoomId>> + 'a> {
        Box::new(self.publicroomids.iter().map(|(bytes, _)| {
            RoomId::parse(utils::string_from_bytes(&bytes).map_err(|_| {
                Error::bad_database(
                    "Room ID in publicroomids is invalid unicode.",
                )
            })?)
            .map_err(|_| {
                Error::bad_database("Room ID in publicroomids is invalid.")
            })
        }))
    }
}
