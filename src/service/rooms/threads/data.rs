use ruma::{
    api::client::threads::get_threads::v1::IncludeThreads, OwnedUserId, RoomId,
    UserId,
};

use crate::{PduEvent, Result};

pub(crate) trait Data: Send + Sync {
    #[allow(clippy::type_complexity)]
    fn threads_until<'a>(
        &'a self,
        user_id: &'a UserId,
        room_id: &'a RoomId,
        until: u64,
        include: &'a IncludeThreads,
    ) -> Result<Box<dyn Iterator<Item = Result<(u64, PduEvent)>> + 'a>>;

    fn update_participants(
        &self,
        root_id: &[u8],
        participants: &[OwnedUserId],
    ) -> Result<()>;
    fn get_participants(
        &self,
        root_id: &[u8],
    ) -> Result<Option<Vec<OwnedUserId>>>;
    fn reset_participants(&self, root_id: &[u8]) -> Result<()>;
}
