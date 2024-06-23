use ruma::{
    api::{
        client::{
            error::ErrorKind,
            profile::{
                get_avatar_url, get_display_name, get_profile, set_avatar_url,
                set_display_name,
            },
        },
        federation::{self, query::get_profile_information::v1::ProfileField},
    },
    events::{
        room::member::RoomMemberEventContent, StateEventType, TimelineEventType,
    },
};
use serde_json::value::to_raw_value;
use tracing::warn;

use crate::{service::pdu::PduBuilder, services, Ar, Error, Ra, Result};

/// # `PUT /_matrix/client/r0/profile/{userId}/displayname`
///
/// Updates the displayname.
///
/// - Also makes sure other users receive the update using presence EDUs
pub(crate) async fn set_displayname_route(
    body: Ar<set_display_name::v3::Request>,
) -> Result<Ra<set_display_name::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    services().users.set_displayname(sender_user, body.displayname.clone())?;

    // Send a new membership event and presence update into all joined rooms
    let all_rooms_joined: Vec<_> = services()
        .rooms
        .state_cache
        .rooms_joined(sender_user)
        .filter_map(Result::ok)
        .map(|room_id| {
            Ok::<_, Error>((
                PduBuilder {
                    event_type: TimelineEventType::RoomMember,
                    content: to_raw_value(&RoomMemberEventContent {
                        displayname: body.displayname.clone(),
                        join_authorized_via_users_server: None,
                        ..serde_json::from_str(
                            services()
                                .rooms
                                .state_accessor
                                .room_state_get(
                                    &room_id,
                                    &StateEventType::RoomMember,
                                    sender_user.as_str(),
                                )?
                                .ok_or_else(|| {
                                    Error::bad_database(
                                        "Tried to send displayname update for \
                                         user not in the room.",
                                    )
                                })?
                                .content
                                .get(),
                        )
                        .map_err(|_| {
                            Error::bad_database(
                                "Database contains invalid PDU.",
                            )
                        })?
                    })
                    .expect("event is valid, we just created it"),
                    unsigned: None,
                    state_key: Some(sender_user.to_string()),
                    redacts: None,
                },
                room_id,
            ))
        })
        .filter_map(Result::ok)
        .collect();

    for (pdu_builder, room_id) in all_rooms_joined {
        let room_token = services()
            .globals
            .roomid_mutex_state
            .lock_key(room_id.clone())
            .await;

        if let Err(error) = services()
            .rooms
            .timeline
            .build_and_append_pdu(pdu_builder, sender_user, &room_token)
            .await
        {
            warn!(%error, "failed to add PDU");
        }
    }

    Ok(Ra(set_display_name::v3::Response {}))
}

/// # `GET /_matrix/client/r0/profile/{userId}/displayname`
///
/// Returns the displayname of the user.
///
/// - If user is on another server: Fetches displayname over federation
pub(crate) async fn get_displayname_route(
    body: Ar<get_display_name::v3::Request>,
) -> Result<Ra<get_display_name::v3::Response>> {
    if body.user_id.server_name() != services().globals.server_name() {
        let response = services()
            .sending
            .send_federation_request(
                body.user_id.server_name(),
                federation::query::get_profile_information::v1::Request {
                    user_id: body.user_id.clone(),
                    field: Some(ProfileField::DisplayName),
                },
            )
            .await?;

        return Ok(Ra(get_display_name::v3::Response {
            displayname: response.displayname,
        }));
    }

    Ok(Ra(get_display_name::v3::Response {
        displayname: services().users.displayname(&body.user_id)?,
    }))
}

/// # `PUT /_matrix/client/r0/profile/{userId}/avatar_url`
///
/// Updates the `avatar_url` and `blurhash`.
///
/// - Also makes sure other users receive the update using presence EDUs
pub(crate) async fn set_avatar_url_route(
    body: Ar<set_avatar_url::v3::Request>,
) -> Result<Ra<set_avatar_url::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    services().users.set_avatar_url(sender_user, body.avatar_url.clone())?;

    services().users.set_blurhash(sender_user, body.blurhash.clone())?;

    // Send a new membership event and presence update into all joined rooms
    let all_joined_rooms: Vec<_> = services()
        .rooms
        .state_cache
        .rooms_joined(sender_user)
        .filter_map(Result::ok)
        .map(|room_id| {
            Ok::<_, Error>((
                PduBuilder {
                    event_type: TimelineEventType::RoomMember,
                    content: to_raw_value(&RoomMemberEventContent {
                        avatar_url: body.avatar_url.clone(),
                        join_authorized_via_users_server: None,
                        ..serde_json::from_str(
                            services()
                                .rooms
                                .state_accessor
                                .room_state_get(
                                    &room_id,
                                    &StateEventType::RoomMember,
                                    sender_user.as_str(),
                                )?
                                .ok_or_else(|| {
                                    Error::bad_database(
                                        "Tried to send displayname update for \
                                         user not in the room.",
                                    )
                                })?
                                .content
                                .get(),
                        )
                        .map_err(|_| {
                            Error::bad_database(
                                "Database contains invalid PDU.",
                            )
                        })?
                    })
                    .expect("event is valid, we just created it"),
                    unsigned: None,
                    state_key: Some(sender_user.to_string()),
                    redacts: None,
                },
                room_id,
            ))
        })
        .filter_map(Result::ok)
        .collect();

    for (pdu_builder, room_id) in all_joined_rooms {
        let room_token = services()
            .globals
            .roomid_mutex_state
            .lock_key(room_id.clone())
            .await;

        if let Err(error) = services()
            .rooms
            .timeline
            .build_and_append_pdu(pdu_builder, sender_user, &room_token)
            .await
        {
            warn!(%error, "failed to add PDU");
        };
    }

    Ok(Ra(set_avatar_url::v3::Response {}))
}

/// # `GET /_matrix/client/r0/profile/{userId}/avatar_url`
///
/// Returns the `avatar_url` and `blurhash` of the user.
///
/// - If user is on another server: Fetches `avatar_url` and `blurhash` over
///   federation
pub(crate) async fn get_avatar_url_route(
    body: Ar<get_avatar_url::v3::Request>,
) -> Result<Ra<get_avatar_url::v3::Response>> {
    if body.user_id.server_name() != services().globals.server_name() {
        let response = services()
            .sending
            .send_federation_request(
                body.user_id.server_name(),
                federation::query::get_profile_information::v1::Request {
                    user_id: body.user_id.clone(),
                    field: Some(ProfileField::AvatarUrl),
                },
            )
            .await?;

        return Ok(Ra(get_avatar_url::v3::Response {
            avatar_url: response.avatar_url,
            blurhash: response.blurhash,
        }));
    }

    Ok(Ra(get_avatar_url::v3::Response {
        avatar_url: services().users.avatar_url(&body.user_id)?,
        blurhash: services().users.blurhash(&body.user_id)?,
    }))
}

/// # `GET /_matrix/client/r0/profile/{userId}`
///
/// Returns the `displayname`, `avatar_url` and `blurhash` of the user.
///
/// - If user is on another server: Fetches profile over federation
pub(crate) async fn get_profile_route(
    body: Ar<get_profile::v3::Request>,
) -> Result<Ra<get_profile::v3::Response>> {
    if body.user_id.server_name() != services().globals.server_name() {
        let response = services()
            .sending
            .send_federation_request(
                body.user_id.server_name(),
                federation::query::get_profile_information::v1::Request {
                    user_id: body.user_id.clone(),
                    field: None,
                },
            )
            .await?;

        return Ok(Ra(get_profile::v3::Response {
            displayname: response.displayname,
            avatar_url: response.avatar_url,
            blurhash: response.blurhash,
        }));
    }

    if !services().users.exists(&body.user_id)? {
        // Return 404 if this user doesn't exist
        return Err(Error::BadRequest(
            ErrorKind::NotFound,
            "Profile was not found.",
        ));
    }

    Ok(Ra(get_profile::v3::Response {
        avatar_url: services().users.avatar_url(&body.user_id)?,
        blurhash: services().users.blurhash(&body.user_id)?,
        displayname: services().users.displayname(&body.user_id)?,
    }))
}
