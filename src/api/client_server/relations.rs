use ruma::{
    api::client::relations::{
        get_relating_events, get_relating_events_with_rel_type,
        get_relating_events_with_rel_type_and_event_type,
    },
    uint,
};

use crate::{
    service::rooms::timeline::PduCount, services, Result, Ruma, RumaResponse,
};

/// # `GET /_matrix/client/r0/rooms/{roomId}/relations/{eventId}/{relType}/{eventType}`
pub(crate) async fn get_relating_events_with_rel_type_and_event_type_route(
    body: Ruma<get_relating_events_with_rel_type_and_event_type::v1::Request>,
) -> Result<
    RumaResponse<
        get_relating_events_with_rel_type_and_event_type::v1::Response,
    >,
> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let from = match body.from.clone() {
        Some(from) => PduCount::try_from_string(&from)?,
        None => match ruma::api::Direction::Backward {
            // TODO: fix ruma so `body.dir` exists
            ruma::api::Direction::Forward => PduCount::MIN,
            ruma::api::Direction::Backward => PduCount::MAX,
        },
    };

    let to = body.to.as_ref().and_then(|t| PduCount::try_from_string(t).ok());

    // Use limit or else 10, with maximum 100
    let limit = body
        .limit
        .map(|x| x.min(uint!(100)))
        .unwrap_or(uint!(10))
        .try_into()
        .expect("0-100 should fit in usize");

    let res = services().rooms.pdu_metadata.paginate_relations_with_filter(
        sender_user,
        &body.room_id,
        &body.event_id,
        Some(&body.event_type),
        Some(&body.rel_type),
        from,
        to,
        limit,
    )?;

    Ok(RumaResponse(
        get_relating_events_with_rel_type_and_event_type::v1::Response {
            chunk: res.chunk,
            next_batch: res.next_batch,
            prev_batch: res.prev_batch,
        },
    ))
}

/// # `GET /_matrix/client/r0/rooms/{roomId}/relations/{eventId}/{relType}`
pub(crate) async fn get_relating_events_with_rel_type_route(
    body: Ruma<get_relating_events_with_rel_type::v1::Request>,
) -> Result<RumaResponse<get_relating_events_with_rel_type::v1::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let from = match body.from.clone() {
        Some(from) => PduCount::try_from_string(&from)?,
        None => match ruma::api::Direction::Backward {
            // TODO: fix ruma so `body.dir` exists
            ruma::api::Direction::Forward => PduCount::MIN,
            ruma::api::Direction::Backward => PduCount::MAX,
        },
    };

    let to = body.to.as_ref().and_then(|t| PduCount::try_from_string(t).ok());

    // Use limit or else 10, with maximum 100
    let limit = body
        .limit
        .map(|x| x.min(uint!(100)))
        .unwrap_or(uint!(10))
        .try_into()
        .expect("0-100 should fit in usize");

    let res = services().rooms.pdu_metadata.paginate_relations_with_filter(
        sender_user,
        &body.room_id,
        &body.event_id,
        None,
        Some(&body.rel_type),
        from,
        to,
        limit,
    )?;

    Ok(RumaResponse(get_relating_events_with_rel_type::v1::Response {
        chunk: res.chunk,
        next_batch: res.next_batch,
        prev_batch: res.prev_batch,
    }))
}

/// # `GET /_matrix/client/r0/rooms/{roomId}/relations/{eventId}`
pub(crate) async fn get_relating_events_route(
    body: Ruma<get_relating_events::v1::Request>,
) -> Result<RumaResponse<get_relating_events::v1::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let from = match body.from.clone() {
        Some(from) => PduCount::try_from_string(&from)?,
        None => match ruma::api::Direction::Backward {
            // TODO: fix ruma so `body.dir` exists
            ruma::api::Direction::Forward => PduCount::MIN,
            ruma::api::Direction::Backward => PduCount::MAX,
        },
    };

    let to = body.to.as_ref().and_then(|t| PduCount::try_from_string(t).ok());

    // Use limit or else 10, with maximum 100
    let limit = body
        .limit
        .map(|x| x.min(uint!(100)))
        .unwrap_or(uint!(10))
        .try_into()
        .expect("0-100 should fit in usize");

    services()
        .rooms
        .pdu_metadata
        .paginate_relations_with_filter(
            sender_user,
            &body.room_id,
            &body.event_id,
            None,
            None,
            from,
            to,
            limit,
        )
        .map(RumaResponse)
}
