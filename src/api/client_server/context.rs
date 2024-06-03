use std::collections::HashSet;

use ruma::{
    api::client::{
        context::get_context, error::ErrorKind, filter::LazyLoadOptions,
    },
    events::StateEventType,
    uint, UInt,
};
use tracing::error;

use crate::{
    services,
    utils::filter::{load_limit, CompiledRoomEventFilter},
    Ar, Error, Ra, Result,
};

/// # `GET /_matrix/client/r0/rooms/{roomId}/context`
///
/// Allows loading room history around an event.
///
/// - Only works if the user is joined (TODO: always allow, but only show events
///   if the user was
/// joined, depending on `history_visibility`)
#[allow(clippy::too_many_lines)]
pub(crate) async fn get_context_route(
    body: Ar<get_context::v3::Request>,
) -> Result<Ra<get_context::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");
    let sender_device =
        body.sender_device.as_ref().expect("user is authenticated");

    let Ok(filter) = CompiledRoomEventFilter::try_from(&body.filter) else {
        return Err(Error::BadRequest(
            ErrorKind::InvalidParam,
            "invalid 'filter' parameter",
        ));
    };

    let (lazy_load_enabled, lazy_load_send_redundant) =
        match &body.filter.lazy_load_options {
            LazyLoadOptions::Enabled {
                include_redundant_members,
            } => (true, *include_redundant_members),
            LazyLoadOptions::Disabled => (false, false),
        };

    let mut lazy_loaded = HashSet::new();

    let base_token =
        services().rooms.timeline.get_pdu_count(&body.event_id)?.ok_or(
            Error::BadRequest(ErrorKind::NotFound, "Base event id not found."),
        )?;

    let base_event = services().rooms.timeline.get_pdu(&body.event_id)?.ok_or(
        Error::BadRequest(ErrorKind::NotFound, "Base event not found."),
    )?;

    let room_id = base_event.room_id.clone();

    if !services().rooms.state_accessor.user_can_see_event(
        sender_user,
        &room_id,
        &body.event_id,
    )? {
        return Err(Error::BadRequest(
            ErrorKind::forbidden(),
            "You don't have permission to view this event.",
        ));
    }

    if !services().rooms.lazy_loading.lazy_load_was_sent_before(
        sender_user,
        sender_device,
        &room_id,
        &base_event.sender,
    )? || lazy_load_send_redundant
    {
        lazy_loaded.insert(base_event.sender.as_str().to_owned());
    }

    let limit: usize = body
        .limit
        .min(body.filter.limit.unwrap_or(UInt::MAX))
        .min(uint!(100))
        .try_into()
        .expect("0-100 should fit in usize");
    let half_limit = limit / 2;

    let base_event = base_event.to_room_event();

    if !filter.room_allowed(&body.room_id) {
        // The spec states that
        //
        // > The filter is only applied to events_before, events_after, and
        // > state. It is not applied to the event itself.
        //
        // so we need to fetch the event before we can early-return after
        // testing the room filter.
        return Ok(Ra(get_context::v3::Response {
            start: None,
            end: None,
            events_before: vec![],
            event: Some(base_event),
            events_after: vec![],
            state: vec![],
        }));
    }

    let mut start_token = None;
    let events_before: Vec<_> = services()
        .rooms
        .timeline
        .pdus_until(sender_user, &room_id, base_token)?
        .take(load_limit(half_limit))
        .filter_map(Result::ok)
        .inspect(|&(count, _)| start_token = Some(count))
        .filter(|(_, pdu)| filter.pdu_event_allowed(pdu))
        .filter(|(_, pdu)| {
            services()
                .rooms
                .state_accessor
                .user_can_see_event(sender_user, &room_id, &pdu.event_id)
                .unwrap_or(false)
        })
        .take(half_limit)
        .collect();

    let start_token = start_token.map(|token| token.stringify());

    for (_, event) in &events_before {
        if !services().rooms.lazy_loading.lazy_load_was_sent_before(
            sender_user,
            sender_device,
            &room_id,
            &event.sender,
        )? || lazy_load_send_redundant
        {
            lazy_loaded.insert(event.sender.as_str().to_owned());
        }
    }

    let events_before: Vec<_> =
        events_before.into_iter().map(|(_, pdu)| pdu.to_room_event()).collect();

    let mut end_token = None;
    let events_after: Vec<_> = services()
        .rooms
        .timeline
        .pdus_after(sender_user, &room_id, base_token)?
        .take(load_limit(half_limit))
        .filter_map(Result::ok)
        .inspect(|&(count, _)| end_token = Some(count))
        .filter(|(_, pdu)| filter.pdu_event_allowed(pdu))
        .filter(|(_, pdu)| {
            services()
                .rooms
                .state_accessor
                .user_can_see_event(sender_user, &room_id, &pdu.event_id)
                .unwrap_or(false)
        })
        .take(half_limit)
        .collect();

    let end_token = end_token.map(|token| token.stringify());

    for (_, event) in &events_after {
        if !services().rooms.lazy_loading.lazy_load_was_sent_before(
            sender_user,
            sender_device,
            &room_id,
            &event.sender,
        )? || lazy_load_send_redundant
        {
            lazy_loaded.insert(event.sender.as_str().to_owned());
        }
    }

    let shortstatehash =
        match services().rooms.state_accessor.pdu_shortstatehash(
            events_after.last().map_or(&*body.event_id, |(_, e)| &*e.event_id),
        )? {
            Some(s) => s,
            None => services()
                .rooms
                .state
                .get_room_shortstatehash(&room_id)?
                .expect("All rooms have state"),
        };

    let state_ids =
        services().rooms.state_accessor.state_full_ids(shortstatehash).await?;

    let events_after: Vec<_> =
        events_after.into_iter().map(|(_, pdu)| pdu.to_room_event()).collect();

    let mut state = Vec::new();

    for (shortstatekey, id) in state_ids {
        let (event_type, state_key) =
            services().rooms.short.get_statekey_from_short(shortstatekey)?;

        if event_type != StateEventType::RoomMember {
            let Some(pdu) = services().rooms.timeline.get_pdu(&id)? else {
                error!("Pdu in state not found: {}", id);
                continue;
            };
            state.push(pdu.to_state_event());
        } else if !lazy_load_enabled || lazy_loaded.contains(&state_key) {
            let Some(pdu) = services().rooms.timeline.get_pdu(&id)? else {
                error!("Pdu in state not found: {}", id);
                continue;
            };
            state.push(pdu.to_state_event());
        }
    }

    let resp = get_context::v3::Response {
        start: start_token,
        end: end_token,
        events_before,
        event: Some(base_event),
        events_after,
        state,
    };

    Ok(Ra(resp))
}
