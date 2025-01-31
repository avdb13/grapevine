use std::collections::BTreeMap;

use ruma::{
    events::{typing::TypingEventContent, SyncEphemeralRoomEvent},
    OwnedRoomId, OwnedUserId, RoomId, UserId,
};
use tokio::sync::{broadcast, RwLock};
use tracing::trace;

use crate::{services, utils, Result};

pub(crate) struct Service {
    // u64 is unix timestamp of timeout
    pub(crate) typing:
        RwLock<BTreeMap<OwnedRoomId, BTreeMap<OwnedUserId, u64>>>,
    // timestamp of the last change to typing users
    pub(crate) last_typing_update: RwLock<BTreeMap<OwnedRoomId, u64>>,
    pub(crate) typing_update_sender: broadcast::Sender<OwnedRoomId>,
}

impl Service {
    /// Sets a user as typing until the timeout timestamp is reached or
    /// `roomtyping_remove` is called.
    #[tracing::instrument(skip(self))]
    pub(crate) async fn typing_add(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        timeout: u64,
    ) -> Result<()> {
        self.typing
            .write()
            .await
            .entry(room_id.to_owned())
            .or_default()
            .insert(user_id.to_owned(), timeout);
        self.last_typing_update
            .write()
            .await
            .insert(room_id.to_owned(), services().globals.next_count()?);
        if self.typing_update_sender.send(room_id.to_owned()).is_err() {
            trace!(
                "receiver found what it was looking for and is no longer \
                 interested"
            );
        }
        Ok(())
    }

    /// Removes a user from typing before the timeout is reached.
    #[tracing::instrument(skip(self))]
    pub(crate) async fn typing_remove(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
    ) -> Result<()> {
        self.typing
            .write()
            .await
            .entry(room_id.to_owned())
            .or_default()
            .remove(user_id);
        self.last_typing_update
            .write()
            .await
            .insert(room_id.to_owned(), services().globals.next_count()?);
        if self.typing_update_sender.send(room_id.to_owned()).is_err() {
            trace!(
                "receiver found what it was looking for and is no longer \
                 interested"
            );
        }
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub(crate) async fn wait_for_update(&self, room_id: &RoomId) -> Result<()> {
        let mut receiver = self.typing_update_sender.subscribe();
        while let Ok(next) = receiver.recv().await {
            if next == room_id {
                break;
            }
        }

        Ok(())
    }

    /// Makes sure that typing events with old timestamps get removed.
    #[tracing::instrument(skip(self, room_id))]
    async fn typings_maintain(&self, room_id: &RoomId) -> Result<()> {
        let current_timestamp = utils::millis_since_unix_epoch();
        let mut removable = Vec::new();
        {
            let typing = self.typing.read().await;
            let Some(room) = typing.get(room_id) else {
                return Ok(());
            };
            for (user, timeout) in room {
                if *timeout < current_timestamp {
                    removable.push(user.clone());
                }
            }
            drop(typing);
        }
        if !removable.is_empty() {
            let typing = &mut self.typing.write().await;
            let room = typing.entry(room_id.to_owned()).or_default();
            for user in removable {
                room.remove(&user);
            }
            self.last_typing_update
                .write()
                .await
                .insert(room_id.to_owned(), services().globals.next_count()?);
            if self.typing_update_sender.send(room_id.to_owned()).is_err() {
                trace!(
                    "receiver found what it was looking for and is no longer \
                     interested"
                );
            }
        }
        Ok(())
    }

    /// Returns the count of the last typing update in this room.
    #[tracing::instrument(skip(self))]
    pub(crate) async fn last_typing_update(
        &self,
        room_id: &RoomId,
    ) -> Result<u64> {
        self.typings_maintain(room_id).await?;
        Ok(self
            .last_typing_update
            .read()
            .await
            .get(room_id)
            .copied()
            .unwrap_or(0))
    }

    /// Returns a new typing EDU.
    #[tracing::instrument(skip(self))]
    pub(crate) async fn typings_all(
        &self,
        room_id: &RoomId,
    ) -> Result<SyncEphemeralRoomEvent<TypingEventContent>> {
        Ok(SyncEphemeralRoomEvent {
            content: TypingEventContent {
                user_ids: self
                    .typing
                    .read()
                    .await
                    .get(room_id)
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default(),
            },
        })
    }
}
