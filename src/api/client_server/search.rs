use std::collections::BTreeMap;

use ruma::{
    api::client::{
        error::ErrorKind,
        search::search_events::{
            self,
            v3::{
                EventContextResult, ResultCategories, ResultRoomEvents,
                SearchResult,
            },
        },
    },
    uint,
};

use crate::{services, Error, Result, Ruma};

/// # `POST /_matrix/client/r0/search`
///
/// Searches rooms for messages.
///
/// - Only works if the user is currently joined to the room (TODO: Respect
///   history visibility)
#[allow(clippy::too_many_lines)]
pub(crate) async fn search_events_route(
    body: Ruma<search_events::v3::Request>,
) -> Result<search_events::v3::Response> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let search_criteria = body.search_categories.room_events.as_ref().unwrap();
    let filter = &search_criteria.filter;

    let room_ids = filter.rooms.clone().unwrap_or_else(|| {
        services()
            .rooms
            .state_cache
            .rooms_joined(sender_user)
            .filter_map(Result::ok)
            .collect()
    });

    // Use limit or else 10, with maximum 100
    let limit = filter
        .limit
        .map(|x| x.min(uint!(100)))
        .unwrap_or(uint!(10))
        .try_into()
        .expect("0-100 should fit in usize");

    let mut searches = Vec::new();

    for room_id in room_ids {
        if !services().rooms.state_cache.is_joined(sender_user, &room_id)? {
            return Err(Error::BadRequest(
                ErrorKind::Forbidden,
                "You don't have permission to view this room.",
            ));
        }

        if let Some(search) = services()
            .rooms
            .search
            .search_pdus(&room_id, &search_criteria.search_term)?
        {
            searches.push(search.0.peekable());
        }
    }

    let skip = match body.next_batch.as_ref().map(|s| s.parse()) {
        Some(Ok(s)) => s,
        Some(Err(_)) => {
            return Err(Error::BadRequest(
                ErrorKind::InvalidParam,
                "Invalid next_batch token.",
            ))
        }
        // Default to the start
        None => 0,
    };

    let mut results = Vec::new();
    for _ in 0..skip + limit {
        if let Some(s) = searches
            .iter_mut()
            .map(|s| (s.peek().cloned(), s))
            .max_by_key(|(peek, _)| peek.clone())
            .and_then(|(_, i)| i.next())
        {
            results.push(s);
        }
    }

    let results: Vec<_> = results
        .iter()
        .filter_map(|result| {
            services()
                .rooms
                .timeline
                .get_pdu_from_id(result)
                .ok()?
                .filter(|pdu| {
                    services()
                        .rooms
                        .state_accessor
                        .user_can_see_event(
                            sender_user,
                            &pdu.room_id,
                            &pdu.event_id,
                        )
                        .unwrap_or(false)
                })
                .map(|pdu| pdu.to_room_event())
        })
        .map(|result| {
            Ok::<_, Error>(SearchResult {
                context: EventContextResult {
                    end: None,
                    events_after: Vec::new(),
                    events_before: Vec::new(),
                    profile_info: BTreeMap::new(),
                    start: None,
                },
                rank: None,
                result: Some(result),
            })
        })
        .filter_map(Result::ok)
        .skip(skip)
        .take(limit)
        .collect();

    let next_batch = if results.len() < limit {
        None
    } else {
        Some((skip + limit).to_string())
    };

    Ok(search_events::v3::Response::new(ResultCategories {
        room_events: ResultRoomEvents {
            count: None,
            // TODO
            groups: BTreeMap::new(),
            next_batch,
            results,
            // TODO
            state: BTreeMap::new(),
            highlights: search_criteria
                .search_term
                .split_terminator(|c: char| !c.is_alphanumeric())
                .map(str::to_lowercase)
                .collect(),
        },
    }))
}
