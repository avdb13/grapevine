mod data;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

pub(crate) use data::Data;
use lru_cache::LruCache;
use ruma::{
    events::{
        room::{
            avatar::RoomAvatarEventContent,
            history_visibility::{
                HistoryVisibility, RoomHistoryVisibilityEventContent,
            },
            member::{MembershipState, RoomMemberEventContent},
            name::RoomNameEventContent,
            power_levels::{RoomPowerLevels, RoomPowerLevelsEventContent},
        },
        StateEventType,
    },
    state_res::Event,
    EventId, JsOption, OwnedRoomId, OwnedServerName, OwnedUserId, RoomId,
    ServerName, UserId,
};
use serde_json::value::to_raw_value;
use tracing::{error, warn};

use crate::{
    observability::{FoundIn, Lookup, METRICS},
    service::{globals::marker, pdu::PduBuilder},
    services,
    utils::on_demand_hashmap::KeyToken,
    Error, PduEvent, Result,
};

pub(crate) struct Service {
    pub(crate) db: &'static dyn Data,
    pub(crate) server_visibility_cache:
        Mutex<LruCache<(OwnedServerName, u64), bool>>,
    pub(crate) user_visibility_cache: Mutex<LruCache<(OwnedUserId, u64), bool>>,
}

impl Service {
    /// Builds a StateMap by iterating over all keys that start
    /// with state_hash, this gives the full state for the given state_hash.
    #[tracing::instrument(skip(self))]
    pub(crate) async fn state_full_ids(
        &self,
        shortstatehash: u64,
    ) -> Result<HashMap<u64, Arc<EventId>>> {
        self.db.state_full_ids(shortstatehash).await
    }

    #[tracing::instrument(skip(self))]
    pub(crate) async fn state_full(
        &self,
        shortstatehash: u64,
    ) -> Result<HashMap<(StateEventType, String), Arc<PduEvent>>> {
        self.db.state_full(shortstatehash).await
    }

    /// Returns a single PDU from `room_id` with key (`event_type`,
    /// `state_key`).
    #[tracing::instrument(skip(self))]
    pub(crate) fn state_get_id(
        &self,
        shortstatehash: u64,
        event_type: &StateEventType,
        state_key: &str,
    ) -> Result<Option<Arc<EventId>>> {
        self.db.state_get_id(shortstatehash, event_type, state_key)
    }

    /// Returns a single PDU from `room_id` with key (`event_type`,
    /// `state_key`).
    #[tracing::instrument(skip(self))]
    pub(crate) fn state_get(
        &self,
        shortstatehash: u64,
        event_type: &StateEventType,
        state_key: &str,
    ) -> Result<Option<Arc<PduEvent>>> {
        self.db.state_get(shortstatehash, event_type, state_key)
    }

    /// Get membership for given user in state
    #[tracing::instrument(skip(self))]
    fn user_membership(
        &self,
        shortstatehash: u64,
        user_id: &UserId,
    ) -> Result<MembershipState> {
        self.state_get(
            shortstatehash,
            &StateEventType::RoomMember,
            user_id.as_str(),
        )?
        .map_or(Ok(MembershipState::Leave), |s| {
            serde_json::from_str(s.content.get())
                .map(|c: RoomMemberEventContent| c.membership)
                .map_err(|_| {
                    Error::bad_database(
                        "Invalid room membership event in database.",
                    )
                })
        })
    }

    /// The user was a joined member at this state (potentially in the past)
    #[tracing::instrument(skip(self), ret(level = "trace"))]
    fn user_was_joined(&self, shortstatehash: u64, user_id: &UserId) -> bool {
        self.user_membership(shortstatehash, user_id)
            .is_ok_and(|s| s == MembershipState::Join)
    }

    /// The user was an invited or joined room member at this state (potentially
    /// in the past)
    #[tracing::instrument(skip(self), ret(level = "trace"))]
    fn user_was_invited(&self, shortstatehash: u64, user_id: &UserId) -> bool {
        self.user_membership(shortstatehash, user_id).is_ok_and(|s| {
            s == MembershipState::Join || s == MembershipState::Invite
        })
    }

    /// Whether a server is allowed to see an event through federation, based on
    /// the room's history_visibility at that event's state.
    #[tracing::instrument(skip(self))]
    pub(crate) fn server_can_see_event(
        &self,
        origin: &ServerName,
        room_id: &RoomId,
        event_id: &EventId,
    ) -> Result<bool> {
        let lookup = Lookup::VisibilityForServer;

        let Some(shortstatehash) = self.pdu_shortstatehash(event_id)? else {
            return Ok(true);
        };

        if let Some(visibility) = self
            .server_visibility_cache
            .lock()
            .unwrap()
            .get_mut(&(origin.to_owned(), shortstatehash))
        {
            METRICS.record_lookup(lookup, FoundIn::Cache);
            return Ok(*visibility);
        }

        let history_visibility = self
            .state_get(
                shortstatehash,
                &StateEventType::RoomHistoryVisibility,
                "",
            )?
            .map_or(Ok(HistoryVisibility::Shared), |s| {
                serde_json::from_str(s.content.get())
                    .map(|c: RoomHistoryVisibilityEventContent| {
                        c.history_visibility
                    })
                    .map_err(|_| {
                        Error::bad_database(
                            "Invalid history visibility event in database.",
                        )
                    })
            })?;

        let mut current_server_members = services()
            .rooms
            .state_cache
            .room_members(room_id)
            .filter_map(Result::ok)
            .filter(|member| member.server_name() == origin);

        let visibility = match history_visibility {
            HistoryVisibility::WorldReadable | HistoryVisibility::Shared => {
                true
            }
            HistoryVisibility::Invited => {
                // Allow if any member on requesting server was AT LEAST
                // invited, else deny
                current_server_members.any(|member| {
                    self.user_was_invited(shortstatehash, &member)
                })
            }
            HistoryVisibility::Joined => {
                // Allow if any member on requested server was joined, else deny
                current_server_members
                    .any(|member| self.user_was_joined(shortstatehash, &member))
            }
            other => {
                error!(kind = %other, "Unknown history visibility");
                false
            }
        };

        METRICS.record_lookup(lookup, FoundIn::Database);
        self.server_visibility_cache
            .lock()
            .unwrap()
            .insert((origin.to_owned(), shortstatehash), visibility);

        Ok(visibility)
    }

    /// Whether a user is allowed to see an event, based on
    /// the room's history_visibility at that event's state.
    #[tracing::instrument(skip(self))]
    pub(crate) fn user_can_see_event(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        event_id: &EventId,
    ) -> Result<bool> {
        let lookup = Lookup::VisibilityForUser;

        let Some(shortstatehash) = self.pdu_shortstatehash(event_id)? else {
            return Ok(true);
        };

        if let Some(visibility) = self
            .user_visibility_cache
            .lock()
            .unwrap()
            .get_mut(&(user_id.to_owned(), shortstatehash))
        {
            METRICS.record_lookup(lookup, FoundIn::Cache);
            return Ok(*visibility);
        }

        let currently_member =
            services().rooms.state_cache.is_joined(user_id, room_id)?;

        let history_visibility = self
            .state_get(
                shortstatehash,
                &StateEventType::RoomHistoryVisibility,
                "",
            )?
            .map_or(Ok(HistoryVisibility::Shared), |s| {
                serde_json::from_str(s.content.get())
                    .map(|c: RoomHistoryVisibilityEventContent| {
                        c.history_visibility
                    })
                    .map_err(|_| {
                        Error::bad_database(
                            "Invalid history visibility event in database.",
                        )
                    })
            })?;

        let visibility = match history_visibility {
            HistoryVisibility::WorldReadable => true,
            HistoryVisibility::Shared => currently_member,
            HistoryVisibility::Invited => {
                // Allow if any member on requesting server was AT LEAST
                // invited, else deny
                self.user_was_invited(shortstatehash, user_id)
            }
            HistoryVisibility::Joined => {
                // Allow if any member on requested server was joined, else deny
                self.user_was_joined(shortstatehash, user_id)
            }
            other => {
                error!(kind = %other, "Unknown history visibility");
                false
            }
        };

        METRICS.record_lookup(lookup, FoundIn::Database);
        self.user_visibility_cache
            .lock()
            .unwrap()
            .insert((user_id.to_owned(), shortstatehash), visibility);

        Ok(visibility)
    }

    /// Whether a user is allowed to see an event, based on
    /// the room's history_visibility at that event's state.
    #[tracing::instrument(skip(self, user_id, room_id))]
    pub(crate) fn user_can_see_state_events(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
    ) -> Result<bool> {
        let currently_member =
            services().rooms.state_cache.is_joined(user_id, room_id)?;

        let history_visibility = self
            .room_state_get(
                room_id,
                &StateEventType::RoomHistoryVisibility,
                "",
            )?
            .map_or(Ok(HistoryVisibility::Shared), |s| {
                serde_json::from_str(s.content.get())
                    .map(|c: RoomHistoryVisibilityEventContent| {
                        c.history_visibility
                    })
                    .map_err(|_| {
                        Error::bad_database(
                            "Invalid history visibility event in database.",
                        )
                    })
            })?;

        Ok(currently_member
            || history_visibility == HistoryVisibility::WorldReadable)
    }

    /// Returns the state hash for this pdu.
    #[tracing::instrument(skip(self))]
    pub(crate) fn pdu_shortstatehash(
        &self,
        event_id: &EventId,
    ) -> Result<Option<u64>> {
        self.db.pdu_shortstatehash(event_id)
    }

    /// Returns the full room state.
    #[tracing::instrument(skip(self))]
    pub(crate) async fn room_state_full(
        &self,
        room_id: &RoomId,
    ) -> Result<HashMap<(StateEventType, String), Arc<PduEvent>>> {
        self.db.room_state_full(room_id).await
    }

    /// Returns a single PDU from `room_id` with key (`event_type`,
    /// `state_key`).
    #[tracing::instrument(skip(self))]
    pub(crate) fn room_state_get_id(
        &self,
        room_id: &RoomId,
        event_type: &StateEventType,
        state_key: &str,
    ) -> Result<Option<Arc<EventId>>> {
        self.db.room_state_get_id(room_id, event_type, state_key)
    }

    /// Returns a single PDU from `room_id` with key (`event_type`,
    /// `state_key`).
    #[tracing::instrument(skip(self))]
    pub(crate) fn room_state_get(
        &self,
        room_id: &RoomId,
        event_type: &StateEventType,
        state_key: &str,
    ) -> Result<Option<Arc<PduEvent>>> {
        self.db.room_state_get(room_id, event_type, state_key)
    }

    #[tracing::instrument(skip(self))]
    pub(crate) fn get_name(&self, room_id: &RoomId) -> Result<Option<String>> {
        self.room_state_get(room_id, &StateEventType::RoomName, "")?.map_or(
            Ok(None),
            |s| {
                serde_json::from_str(s.content.get())
                    .map(|c: RoomNameEventContent| Some(c.name))
                    .map_err(|error| {
                        error!(%error, "Invalid room name event in database");
                        Error::BadDatabase(
                            "Invalid room name event in database.",
                        )
                    })
            },
        )
    }

    #[tracing::instrument(skip(self))]
    pub(crate) fn get_avatar(
        &self,
        room_id: &RoomId,
    ) -> Result<JsOption<RoomAvatarEventContent>> {
        self.room_state_get(room_id, &StateEventType::RoomAvatar, "")?.map_or(
            Ok(JsOption::Undefined),
            |s| {
                serde_json::from_str(s.content.get()).map_err(|_| {
                    Error::bad_database(
                        "Invalid room avatar event in database.",
                    )
                })
            },
        )
    }

    // Allowed because this function uses `services()`
    #[allow(clippy::unused_self)]
    #[tracing::instrument(skip(self), ret(level = "trace"))]
    pub(crate) fn user_can_invite(
        &self,
        room_id: &KeyToken<OwnedRoomId, marker::State>,
        sender: &UserId,
        target_user: &UserId,
    ) -> bool {
        let content =
            to_raw_value(&RoomMemberEventContent::new(MembershipState::Invite))
                .expect("Event content always serializes");

        let new_event = PduBuilder {
            event_type: ruma::events::TimelineEventType::RoomMember,
            content,
            unsigned: None,
            state_key: Some(target_user.into()),
            redacts: None,
        };

        services()
            .rooms
            .timeline
            .create_hash_and_sign_event(new_event, sender, room_id)
            .is_ok()
    }

    #[tracing::instrument(skip(self))]
    pub(crate) fn get_member(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<Option<RoomMemberEventContent>> {
        self.room_state_get(
            room_id,
            &StateEventType::RoomMember,
            user_id.as_str(),
        )?
        .map_or(Ok(None), |s| {
            serde_json::from_str(s.content.get()).map_err(|_| {
                Error::bad_database("Invalid room member event in database.")
            })
        })
    }

    /// Checks if a given user can redact a given event
    ///
    /// If `federation` is `true`, it allows redaction events from any user of
    /// the same server as the original event sender, [as required by room
    /// versions >= v3](https://spec.matrix.org/v1.10/rooms/v11/#handling-redactions)
    #[tracing::instrument(skip(self))]
    pub(crate) fn user_can_redact(
        &self,
        redacts: &EventId,
        sender: &UserId,
        room_id: &RoomId,
        federation: bool,
    ) -> Result<bool> {
        self.room_state_get(room_id, &StateEventType::RoomPowerLevels, "")?
            .map_or_else(
                // Falling back on m.room.create to judge power levels
                || {
                    if let Some(pdu) = self.room_state_get(
                        room_id,
                        &StateEventType::RoomCreate,
                        "",
                    )? {
                        Ok(pdu.sender == sender
                            || if let Ok(Some(pdu)) =
                                services().rooms.timeline.get_pdu(redacts)
                            {
                                pdu.sender == sender
                            } else {
                                false
                            })
                    } else {
                        Err(Error::bad_database(
                            "No m.room.power_levels or m.room.create events \
                             in database for room",
                        ))
                    }
                },
                |e| {
                    serde_json::from_str(e.content.get())
                        .map(|c: RoomPowerLevelsEventContent| c.into())
                        .map(|e: RoomPowerLevels| {
                            e.user_can_redact_event_of_other(sender)
                                || e.user_can_redact_own_event(sender)
                                    && if let Ok(Some(pdu)) = services()
                                        .rooms
                                        .timeline
                                        .get_pdu(redacts)
                                    {
                                        if federation {
                                            pdu.sender().server_name()
                                                == sender.server_name()
                                        } else {
                                            pdu.sender == sender
                                        }
                                    } else {
                                        false
                                    }
                        })
                        .map_err(|_| {
                            Error::bad_database(
                                "Invalid m.room.power_levels event in database",
                            )
                        })
                },
            )
    }
}
