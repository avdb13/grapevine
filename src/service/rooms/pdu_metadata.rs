mod data;
use std::sync::Arc;

pub(crate) use data::Data;
use ruma::{
    api::client::relations::get_relating_events,
    events::{relation::RelationType, TimelineEventType},
    EventId, RoomId, UserId,
};
use serde::Deserialize;

use super::timeline::PduCount;
use crate::{services, PduEvent, Result};

pub(crate) struct Service {
    pub(crate) db: &'static dyn Data,
}

#[derive(Clone, Debug, Deserialize)]
struct ExtractRelType {
    rel_type: RelationType,
}
#[derive(Clone, Debug, Deserialize)]
struct ExtractRelatesToEventId {
    #[serde(rename = "m.relates_to")]
    relates_to: ExtractRelType,
}

impl Service {
    #[tracing::instrument(skip(self, from, to))]
    pub(crate) fn add_relation(
        &self,
        from: PduCount,
        to: PduCount,
    ) -> Result<()> {
        match (from, to) {
            (PduCount::Normal(f), PduCount::Normal(t)) => {
                self.db.add_relation(f, t)
            }
            _ => {
                // TODO: Relations with backfilled pdus

                Ok(())
            }
        }
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        // Allowed because this function uses `services()`
        clippy::unused_self,
    )]
    #[tracing::instrument(skip(self))]
    pub(crate) fn paginate_relations_with_filter(
        &self,
        sender_user: &UserId,
        room_id: &RoomId,
        target: &EventId,
        filter_event_type: Option<&TimelineEventType>,
        filter_rel_type: Option<&RelationType>,
        from: PduCount,
        to: Option<PduCount>,
        limit: usize,
    ) -> Result<get_relating_events::v1::Response> {
        let next_token;

        //TODO: Fix ruma: match body.dir {
        match ruma::api::Direction::Backward {
            ruma::api::Direction::Forward => {
                // TODO: should be relations_after
                let events_after: Vec<_> = services()
                    .rooms
                    .pdu_metadata
                    .relations_until(sender_user, room_id, target, from)?
                    .filter(|r| {
                        r.as_ref().map_or(true, |(_, pdu)| {
                            filter_event_type
                                .as_ref()
                                .map_or(true, |t| &&pdu.kind == t)
                                && if let Ok(content) = serde_json::from_str::<
                                    ExtractRelatesToEventId,
                                >(
                                    pdu.content.get()
                                ) {
                                    filter_rel_type.as_ref().map_or(true, |r| {
                                        &&content.relates_to.rel_type == r
                                    })
                                } else {
                                    false
                                }
                        })
                    })
                    .take(limit)
                    .filter_map(Result::ok)
                    .filter(|(_, pdu)| {
                        services()
                            .rooms
                            .state_accessor
                            .user_can_see_event(
                                sender_user,
                                room_id,
                                &pdu.event_id,
                            )
                            .unwrap_or(false)
                    })
                    .take_while(|&(k, _)| Some(k) != to)
                    .collect();

                next_token =
                    events_after.last().map(|(count, _)| count).copied();

                // Reversed because relations are always most recent first
                let events_after: Vec<_> = events_after
                    .into_iter()
                    .rev()
                    .map(|(_, pdu)| pdu.to_message_like_event())
                    .collect();

                Ok(get_relating_events::v1::Response {
                    chunk: events_after,
                    next_batch: next_token.map(|t| t.stringify()),
                    prev_batch: Some(from.stringify()),
                    // TODO
                    recursion_depth: None,
                })
            }
            ruma::api::Direction::Backward => {
                let events_before: Vec<_> = services()
                    .rooms
                    .pdu_metadata
                    .relations_until(sender_user, room_id, target, from)?
                    .filter(|r| {
                        r.as_ref().map_or(true, |(_, pdu)| {
                            filter_event_type
                                .as_ref()
                                .map_or(true, |t| &&pdu.kind == t)
                                && if let Ok(content) = serde_json::from_str::<
                                    ExtractRelatesToEventId,
                                >(
                                    pdu.content.get()
                                ) {
                                    filter_rel_type.as_ref().map_or(true, |r| {
                                        &&content.relates_to.rel_type == r
                                    })
                                } else {
                                    false
                                }
                        })
                    })
                    .take(limit)
                    .filter_map(Result::ok)
                    .filter(|(_, pdu)| {
                        services()
                            .rooms
                            .state_accessor
                            .user_can_see_event(
                                sender_user,
                                room_id,
                                &pdu.event_id,
                            )
                            .unwrap_or(false)
                    })
                    .take_while(|&(k, _)| Some(k) != to)
                    .collect();

                next_token =
                    events_before.last().map(|(count, _)| count).copied();

                let events_before: Vec<_> = events_before
                    .into_iter()
                    .map(|(_, pdu)| pdu.to_message_like_event())
                    .collect();

                Ok(get_relating_events::v1::Response {
                    chunk: events_before,
                    next_batch: next_token.map(|t| t.stringify()),
                    prev_batch: Some(from.stringify()),
                    // TODO
                    recursion_depth: None,
                })
            }
        }
    }

    #[tracing::instrument(skip_all)]
    pub(crate) fn relations_until<'a>(
        &'a self,
        user_id: &'a UserId,
        room_id: &'a RoomId,
        target: &'a EventId,
        until: PduCount,
    ) -> Result<impl Iterator<Item = Result<(PduCount, PduEvent)>> + 'a> {
        let room_id =
            services().rooms.short.get_or_create_shortroomid(room_id)?;
        let target = match services().rooms.timeline.get_pdu_count(target)? {
            Some(PduCount::Normal(c)) => c,
            // TODO: Support backfilled relations
            // This will result in an empty iterator
            _ => 0,
        };
        self.db.relations_until(user_id, room_id, target, until)
    }

    #[tracing::instrument(skip(self, room_id, event_ids))]
    pub(crate) fn mark_as_referenced(
        &self,
        room_id: &RoomId,
        event_ids: &[Arc<EventId>],
    ) -> Result<()> {
        self.db.mark_as_referenced(room_id, event_ids)
    }

    #[tracing::instrument(skip(self))]
    pub(crate) fn is_event_referenced(
        &self,
        room_id: &RoomId,
        event_id: &EventId,
    ) -> Result<bool> {
        self.db.is_event_referenced(room_id, event_id)
    }

    #[tracing::instrument(skip(self))]
    pub(crate) fn mark_event_soft_failed(
        &self,
        event_id: &EventId,
    ) -> Result<()> {
        self.db.mark_event_soft_failed(event_id)
    }

    #[tracing::instrument(skip(self))]
    pub(crate) fn is_event_soft_failed(
        &self,
        event_id: &EventId,
    ) -> Result<bool> {
        self.db.is_event_soft_failed(event_id)
    }
}
