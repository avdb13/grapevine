use std::collections::BTreeMap;

use ruma::{
    api::client::tag::{create_tag, delete_tag, get_tags},
    events::{
        tag::{TagEvent, TagEventContent},
        RoomAccountDataEventType,
    },
};

use crate::{services, Ar, Error, Ra, Result};

/// # `PUT /_matrix/client/r0/user/{userId}/rooms/{roomId}/tags/{tag}`
///
/// Adds a tag to the room.
///
/// - Inserts the tag into the tag event of the room account data.
pub(crate) async fn update_tag_route(
    body: Ar<create_tag::v3::Request>,
) -> Result<Ra<create_tag::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let event = services().account_data.get(
        Some(&body.room_id),
        sender_user,
        RoomAccountDataEventType::Tag,
    )?;

    let mut tags_event = event.map_or_else(
        || {
            Ok(TagEvent {
                content: TagEventContent {
                    tags: BTreeMap::new(),
                },
            })
        },
        |e| {
            serde_json::from_str(e.get()).map_err(|_| {
                Error::bad_database("Invalid account data event in db.")
            })
        },
    )?;

    tags_event
        .content
        .tags
        .insert(body.tag.clone().into(), body.tag_info.clone());

    services().account_data.update(
        Some(&body.room_id),
        sender_user,
        RoomAccountDataEventType::Tag,
        &serde_json::to_value(tags_event).expect("to json value always works"),
    )?;

    Ok(Ra(create_tag::v3::Response {}))
}

/// # `DELETE /_matrix/client/r0/user/{userId}/rooms/{roomId}/tags/{tag}`
///
/// Deletes a tag from the room.
///
/// - Removes the tag from the tag event of the room account data.
pub(crate) async fn delete_tag_route(
    body: Ar<delete_tag::v3::Request>,
) -> Result<Ra<delete_tag::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let event = services().account_data.get(
        Some(&body.room_id),
        sender_user,
        RoomAccountDataEventType::Tag,
    )?;

    let mut tags_event = event.map_or_else(
        || {
            Ok(TagEvent {
                content: TagEventContent {
                    tags: BTreeMap::new(),
                },
            })
        },
        |e| {
            serde_json::from_str(e.get()).map_err(|_| {
                Error::bad_database("Invalid account data event in db.")
            })
        },
    )?;

    tags_event.content.tags.remove(&body.tag.clone().into());

    services().account_data.update(
        Some(&body.room_id),
        sender_user,
        RoomAccountDataEventType::Tag,
        &serde_json::to_value(tags_event).expect("to json value always works"),
    )?;

    Ok(Ra(delete_tag::v3::Response {}))
}

/// # `GET /_matrix/client/r0/user/{userId}/rooms/{roomId}/tags`
///
/// Returns tags on the room.
///
/// - Gets the tag event of the room account data.
pub(crate) async fn get_tags_route(
    body: Ar<get_tags::v3::Request>,
) -> Result<Ra<get_tags::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let event = services().account_data.get(
        Some(&body.room_id),
        sender_user,
        RoomAccountDataEventType::Tag,
    )?;

    let tags_event = event.map_or_else(
        || {
            Ok(TagEvent {
                content: TagEventContent {
                    tags: BTreeMap::new(),
                },
            })
        },
        |e| {
            serde_json::from_str(e.get()).map_err(|_| {
                Error::bad_database("Invalid account data event in db.")
            })
        },
    )?;

    Ok(Ra(get_tags::v3::Response {
        tags: tags_event.content.tags,
    }))
}
