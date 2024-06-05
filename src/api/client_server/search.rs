use std::{borrow::Cow, collections::BTreeMap};

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
    uint, UInt,
};

use crate::{
    services,
    utils::filter::{AllowDenyList, CompiledRoomEventFilter},
    Ar, Error, Ra, Result,
};

/// # `POST /_matrix/client/r0/search`
///
/// Searches rooms for messages.
///
/// - Only works if the user is currently joined to the room (TODO: Respect
///   history visibility)
#[allow(clippy::too_many_lines)]
pub(crate) async fn search_events_route(
    body: Ar<search_events::v3::Request>,
) -> Result<Ra<search_events::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let search_criteria = body.search_categories.room_events.as_ref().unwrap();
    let filter = &search_criteria.filter;
    let Ok(compiled_filter) = CompiledRoomEventFilter::try_from(filter) else {
        return Err(Error::BadRequest(
            ErrorKind::InvalidParam,
            "invalid 'filter' parameter",
        ));
    };

    let mut room_ids = vec![];
    if let AllowDenyList::Allow(allow_set) = &compiled_filter.rooms {
        for &room_id in allow_set {
            if services().rooms.state_cache.is_joined(sender_user, room_id)? {
                room_ids.push(Cow::Borrowed(room_id));
            }
        }
    } else {
        for result in services().rooms.state_cache.rooms_joined(sender_user) {
            let room_id = result?;
            if compiled_filter.rooms.allowed(&room_id) {
                room_ids.push(Cow::Owned(room_id));
            }
        }
    }

    // Use limit or else 10, with maximum 100
    let limit = filter
        .limit
        .map(|x| x.min(uint!(100)))
        .unwrap_or(uint!(10))
        .try_into()
        .expect("0-100 should fit in usize");

    let mut searches = Vec::new();

    for room_id in room_ids {
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
        .skip(skip)
        .filter_map(|result| {
            services()
                .rooms
                .timeline
                .get_pdu_from_id(result)
                .ok()?
                .filter(|pdu| compiled_filter.pdu_event_allowed(pdu))
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
        .take(limit)
        .collect();

    let more_unloaded_results = searches.iter_mut().any(|s| s.peek().is_some());
    let next_batch = more_unloaded_results.then(|| (skip + limit).to_string());

    Ok(Ra(search_events::v3::Response::new(ResultCategories {
        room_events: ResultRoomEvents {
            // TODO(compat): this is not a good estimate of the total number of
            // results. we should just be returning None, but
            // element incorrectly relies on this field. Switch back
            // to None when [1] is fixed
            //
            // [1]: https://github.com/element-hq/element-web/issues/27517
            count: Some(results.len().try_into().unwrap_or(UInt::MAX)),
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
    })))
}
