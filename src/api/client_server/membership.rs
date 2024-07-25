use std::{
    collections::{hash_map::Entry, BTreeMap, HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use ruma::{
    api::{
        client::{
            error::ErrorKind,
            membership::{
                ban_user, forget_room, get_member_events, invite_user,
                join_room_by_id, join_room_by_id_or_alias, joined_members,
                joined_rooms, kick_user, leave_room, unban_user,
                ThirdPartySigned,
            },
        },
        federation::{self, membership::create_invite},
    },
    canonical_json::to_canonical_value,
    events::{
        room::{
            join_rules::{AllowRule, JoinRule, RoomJoinRulesEventContent},
            member::{MembershipState, RoomMemberEventContent},
        },
        StateEventType, TimelineEventType,
    },
    state_res, CanonicalJsonObject, CanonicalJsonValue, EventId,
    MilliSecondsSinceUnixEpoch, OwnedEventId, OwnedRoomId, OwnedServerName,
    OwnedUserId, RoomId, RoomVersionId, UserId,
};
use serde_json::{
    value::{to_raw_value, RawValue as RawJsonValue},
    Value,
};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use super::get_alias_helper;
use crate::{
    service::{
        globals::SigningKeys,
        pdu::{gen_event_id_canonical_json, PduBuilder},
    },
    services, utils, Ar, Error, PduEvent, Ra, Result,
};

/// # `POST /_matrix/client/r0/rooms/{roomId}/join`
///
/// Tries to join the sender user into a room.
///
/// - If the server knowns about this room: creates the join event and does auth
///   rules locally
/// - If the server does not know about the room: asks other servers over
///   federation
pub(crate) async fn join_room_by_id_route(
    body: Ar<join_room_by_id::v3::Request>,
) -> Result<Ra<join_room_by_id::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    // There is no body.server_name for /roomId/join
    let mut servers = Vec::new();
    if let Some(v) = find_participating_servers(sender_user, &body.room_id)? {
        servers.extend(v);
    }

    if let Some(server_name) = body.room_id.server_name() {
        servers.push(server_name.to_owned());
    }

    join_room_by_id_helper(
        body.sender_user.as_deref(),
        &body.room_id,
        body.reason.clone(),
        &servers,
        body.third_party_signed.as_ref(),
    )
    .await
    .map(Ra)
}

/// # `POST /_matrix/client/r0/join/{roomIdOrAlias}`
///
/// Tries to join the sender user into a room.
///
/// - If the server knowns about this room: creates the join event and does auth
///   rules locally
/// - If the server does not know about the room: asks other servers over
///   federation
pub(crate) async fn join_room_by_id_or_alias_route(
    body: Ar<join_room_by_id_or_alias::v3::Request>,
) -> Result<Ra<join_room_by_id_or_alias::v3::Response>> {
    let sender_user =
        body.sender_user.as_deref().expect("user is authenticated");
    let body = body.body;

    let (servers, room_id) = match OwnedRoomId::try_from(body.room_id_or_alias)
    {
        Ok(room_id) => {
            let mut servers = body.server_name.clone();

            if let Some(v) = find_participating_servers(sender_user, &room_id)?
            {
                servers.extend(v);
            }

            if let Some(server_name) = room_id.server_name() {
                servers.push(server_name.to_owned());
            }

            (servers, room_id)
        }
        Err(room_alias) => {
            let response = get_alias_helper(room_alias).await?;

            (response.servers, response.room_id)
        }
    };

    let join_room_response = join_room_by_id_helper(
        Some(sender_user),
        &room_id,
        body.reason.clone(),
        &servers,
        body.third_party_signed.as_ref(),
    )
    .await?;

    Ok(Ra(join_room_by_id_or_alias::v3::Response {
        room_id: join_room_response.room_id,
    }))
}

/// # `POST /_matrix/client/r0/rooms/{roomId}/leave`
///
/// Tries to leave the sender user from a room.
///
/// - This should always work if the user is currently joined.
pub(crate) async fn leave_room_route(
    body: Ar<leave_room::v3::Request>,
) -> Result<Ra<leave_room::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    leave_room(sender_user, &body.room_id, body.reason.clone()).await?;

    Ok(Ra(leave_room::v3::Response::new()))
}

/// # `POST /_matrix/client/r0/rooms/{roomId}/invite`
///
/// Tries to send an invite event into the room.
pub(crate) async fn invite_user_route(
    body: Ar<invite_user::v3::Request>,
) -> Result<Ra<invite_user::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    if let invite_user::v3::InvitationRecipient::UserId {
        user_id,
    } = &body.recipient
    {
        invite_helper(
            sender_user,
            user_id,
            &body.room_id,
            body.reason.clone(),
            false,
        )
        .await?;
        Ok(Ra(invite_user::v3::Response {}))
    } else {
        Err(Error::BadRequest(ErrorKind::NotFound, "User not found."))
    }
}

/// # `POST /_matrix/client/r0/rooms/{roomId}/kick`
///
/// Tries to send a kick event into the room.
pub(crate) async fn kick_user_route(
    body: Ar<kick_user::v3::Request>,
) -> Result<Ra<kick_user::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let mut event: RoomMemberEventContent = serde_json::from_str(
        services()
            .rooms
            .state_accessor
            .room_state_get(
                &body.room_id,
                &StateEventType::RoomMember,
                body.user_id.as_ref(),
            )?
            .ok_or(Error::BadRequest(
                ErrorKind::BadState,
                "Cannot kick member that's not in the room.",
            ))?
            .content
            .get(),
    )
    .map_err(|_| Error::bad_database("Invalid member event in database."))?;

    event.membership = MembershipState::Leave;
    event.reason.clone_from(&body.reason);

    let room_token = services()
        .globals
        .roomid_mutex_state
        .lock_key(body.room_id.clone())
        .await;

    services()
        .rooms
        .timeline
        .build_and_append_pdu(
            PduBuilder {
                event_type: TimelineEventType::RoomMember,
                content: to_raw_value(&event)
                    .expect("event is valid, we just created it"),
                unsigned: None,
                state_key: Some(body.user_id.to_string()),
                redacts: None,
            },
            sender_user,
            &room_token,
        )
        .await?;

    drop(room_token);

    Ok(Ra(kick_user::v3::Response::new()))
}

/// # `POST /_matrix/client/r0/rooms/{roomId}/ban`
///
/// Tries to send a ban event into the room.
pub(crate) async fn ban_user_route(
    body: Ar<ban_user::v3::Request>,
) -> Result<Ra<ban_user::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let event = services()
        .rooms
        .state_accessor
        .room_state_get(
            &body.room_id,
            &StateEventType::RoomMember,
            body.user_id.as_ref(),
        )?
        .map_or(
            Ok(RoomMemberEventContent {
                membership: MembershipState::Ban,
                displayname: services().users.displayname(&body.user_id)?,
                avatar_url: services().users.avatar_url(&body.user_id)?,
                is_direct: None,
                third_party_invite: None,
                blurhash: services().users.blurhash(&body.user_id)?,
                reason: body.reason.clone(),
                join_authorized_via_users_server: None,
            }),
            |event| {
                serde_json::from_str(event.content.get())
                    .map(|event: RoomMemberEventContent| {
                        RoomMemberEventContent {
                            membership: MembershipState::Ban,
                            join_authorized_via_users_server: None,
                            ..event
                        }
                    })
                    .map_err(|_| {
                        Error::bad_database("Invalid member event in database.")
                    })
            },
        )?;

    let room_token = services()
        .globals
        .roomid_mutex_state
        .lock_key(body.room_id.clone())
        .await;

    services()
        .rooms
        .timeline
        .build_and_append_pdu(
            PduBuilder {
                event_type: TimelineEventType::RoomMember,
                content: to_raw_value(&event)
                    .expect("event is valid, we just created it"),
                unsigned: None,
                state_key: Some(body.user_id.to_string()),
                redacts: None,
            },
            sender_user,
            &room_token,
        )
        .await?;

    drop(room_token);

    Ok(Ra(ban_user::v3::Response::new()))
}

/// # `POST /_matrix/client/r0/rooms/{roomId}/unban`
///
/// Tries to send an unban event into the room.
pub(crate) async fn unban_user_route(
    body: Ar<unban_user::v3::Request>,
) -> Result<Ra<unban_user::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let mut event: RoomMemberEventContent = serde_json::from_str(
        services()
            .rooms
            .state_accessor
            .room_state_get(
                &body.room_id,
                &StateEventType::RoomMember,
                body.user_id.as_ref(),
            )?
            .ok_or(Error::BadRequest(
                ErrorKind::BadState,
                "Cannot unban a user who is not banned.",
            ))?
            .content
            .get(),
    )
    .map_err(|_| Error::bad_database("Invalid member event in database."))?;

    event.membership = MembershipState::Leave;
    event.reason.clone_from(&body.reason);

    let room_token = services()
        .globals
        .roomid_mutex_state
        .lock_key(body.room_id.clone())
        .await;

    services()
        .rooms
        .timeline
        .build_and_append_pdu(
            PduBuilder {
                event_type: TimelineEventType::RoomMember,
                content: to_raw_value(&event)
                    .expect("event is valid, we just created it"),
                unsigned: None,
                state_key: Some(body.user_id.to_string()),
                redacts: None,
            },
            sender_user,
            &room_token,
        )
        .await?;

    drop(room_token);

    Ok(Ra(unban_user::v3::Response::new()))
}

/// # `POST /_matrix/client/r0/rooms/{roomId}/forget`
///
/// Forgets about a room.
///
/// - If the sender user currently left the room: Stops sender user from
///   receiving information about the room
///
/// Note: Other devices of the user have no way of knowing the room was
/// forgotten, so this has to be called from every device
pub(crate) async fn forget_room_route(
    body: Ar<forget_room::v3::Request>,
) -> Result<Ra<forget_room::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    services().rooms.state_cache.forget(&body.room_id, sender_user)?;

    Ok(Ra(forget_room::v3::Response::new()))
}

/// # `POST /_matrix/client/r0/joined_rooms`
///
/// Lists all rooms the user has joined.
pub(crate) async fn joined_rooms_route(
    body: Ar<joined_rooms::v3::Request>,
) -> Result<Ra<joined_rooms::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    Ok(Ra(joined_rooms::v3::Response {
        joined_rooms: services()
            .rooms
            .state_cache
            .rooms_joined(sender_user)
            .filter_map(Result::ok)
            .collect(),
    }))
}

/// # `POST /_matrix/client/r0/rooms/{roomId}/members`
///
/// Lists all joined users in a room (TODO: at a specific point in time, with a
/// specific membership).
///
/// - Only works if the user is currently joined
pub(crate) async fn get_member_events_route(
    body: Ar<get_member_events::v3::Request>,
) -> Result<Ra<get_member_events::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    if !services()
        .rooms
        .state_accessor
        .user_can_see_state_events(sender_user, &body.room_id)?
    {
        return Err(Error::BadRequest(
            ErrorKind::forbidden(),
            "You don't have permission to view this room.",
        ));
    }

    Ok(Ra(get_member_events::v3::Response {
        chunk: services()
            .rooms
            .state_accessor
            .room_state_full(&body.room_id)
            .await?
            .iter()
            .filter(|(key, _)| key.0 == StateEventType::RoomMember)
            .map(|(_, pdu)| pdu.to_member_event())
            .collect(),
    }))
}

/// # `POST /_matrix/client/r0/rooms/{roomId}/joined_members`
///
/// Lists all members of a room.
///
/// - The sender user must be in the room
/// - TODO: An appservice just needs a puppet joined
pub(crate) async fn joined_members_route(
    body: Ar<joined_members::v3::Request>,
) -> Result<Ra<joined_members::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    if !services()
        .rooms
        .state_accessor
        .user_can_see_state_events(sender_user, &body.room_id)?
    {
        return Err(Error::BadRequest(
            ErrorKind::forbidden(),
            "You don't have permission to view this room.",
        ));
    }

    let mut joined = BTreeMap::new();
    for user_id in services()
        .rooms
        .state_cache
        .room_members(&body.room_id)
        .filter_map(Result::ok)
    {
        let display_name = services().users.displayname(&user_id)?;
        let avatar_url = services().users.avatar_url(&user_id)?;

        joined.insert(
            user_id,
            joined_members::v3::RoomMember {
                display_name,
                avatar_url,
            },
        );
    }

    Ok(Ra(joined_members::v3::Response {
        joined,
    }))
}

/// Tries to find servers already participating in this room by
/// inspecting its stripped state events, if any.
pub(crate) fn find_participating_servers(
    sender_user: &UserId,
    room_id: &RoomId,
) -> Result<Option<impl Iterator<Item = OwnedServerName>>> {
    let Some(invite_state) =
        services().rooms.state_cache.invite_state(sender_user, room_id)?
    else {
        return Ok(None);
    };

    let servers = invite_state
        .into_iter()
        .filter_map(|event| event.cast_ref::<Value>().deserialize().ok())
        .filter_map(|event| {
            event.get("sender").and_then(Value::as_str).map(str::to_owned)
        })
        .filter_map(|event| UserId::parse(event).ok())
        .map(|user_id| user_id.server_name().to_owned());

    Ok(Some(servers))
}

#[allow(clippy::too_many_lines)]
#[tracing::instrument(skip(reason, _third_party_signed))]
async fn join_room_by_id_helper(
    sender_user: Option<&UserId>,
    room_id: &RoomId,
    reason: Option<String>,
    servers: &[OwnedServerName],
    _third_party_signed: Option<&ThirdPartySigned>,
) -> Result<join_room_by_id::v3::Response> {
    let sender_user = sender_user.expect("user is authenticated");

    let room_token = services()
        .globals
        .roomid_mutex_state
        .lock_key(room_id.to_owned())
        .await;

    // Ask a remote server if we are not participating in this room
    if services()
        .rooms
        .state_cache
        .server_in_room(services().globals.server_name(), room_id)?
    {
        info!("We can join locally");

        let join_rules_event = services().rooms.state_accessor.room_state_get(
            room_id,
            &StateEventType::RoomJoinRules,
            "",
        )?;

        let join_rules_event_content: Option<RoomJoinRulesEventContent> =
            join_rules_event
                .as_ref()
                .map(|join_rules_event| {
                    serde_json::from_str(join_rules_event.content.get())
                        .map_err(|error| {
                            warn!(%error, "Invalid join rules event");
                            Error::bad_database(
                                "Invalid join rules event in db.",
                            )
                        })
                })
                .transpose()?;

        let restriction_rooms = match join_rules_event_content {
            Some(RoomJoinRulesEventContent {
                join_rule:
                    JoinRule::Restricted(restricted)
                    | JoinRule::KnockRestricted(restricted),
            }) => restricted
                .allow
                .into_iter()
                .filter_map(|a| match a {
                    AllowRule::RoomMembership(r) => Some(r.room_id),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };

        let authorized_user =
            if restriction_rooms.iter().any(|restriction_room_id| {
                services()
                    .rooms
                    .state_cache
                    .is_joined(sender_user, restriction_room_id)
                    .unwrap_or(false)
            }) {
                let mut auth_user = None;
                for user in services()
                    .rooms
                    .state_cache
                    .room_members(room_id)
                    .filter_map(Result::ok)
                    .collect::<Vec<_>>()
                {
                    if user.server_name() == services().globals.server_name()
                        && services().rooms.state_accessor.user_can_invite(
                            &room_token,
                            &user,
                            sender_user,
                        )
                    {
                        auth_user = Some(user);
                        break;
                    }
                }
                auth_user
            } else {
                None
            };

        let event = RoomMemberEventContent {
            membership: MembershipState::Join,
            displayname: services().users.displayname(sender_user)?,
            avatar_url: services().users.avatar_url(sender_user)?,
            is_direct: None,
            third_party_invite: None,
            blurhash: services().users.blurhash(sender_user)?,
            reason: reason.clone(),
            join_authorized_via_users_server: authorized_user,
        };

        // Try normal join first
        let error = match services()
            .rooms
            .timeline
            .build_and_append_pdu(
                PduBuilder {
                    event_type: TimelineEventType::RoomMember,
                    content: to_raw_value(&event)
                        .expect("event is valid, we just created it"),
                    unsigned: None,
                    state_key: Some(sender_user.to_string()),
                    redacts: None,
                },
                sender_user,
                &room_token,
            )
            .await
        {
            Ok(_event_id) => {
                return Ok(join_room_by_id::v3::Response::new(
                    room_id.to_owned(),
                ))
            }
            Err(e) => e,
        };

        if restriction_rooms.is_empty()
            && servers.iter().any(|s| *s != services().globals.server_name())
        {
            return Err(error);
        }

        info!(
            "We couldn't do the join locally, maybe federation can help to \
             satisfy the restricted join requirements"
        );
        let (make_join_response, remote_server) =
            make_join_request(sender_user, room_id, servers).await?;

        let room_version_id = match make_join_response.room_version {
            Some(room_version_id)
                if services()
                    .globals
                    .supported_room_versions()
                    .contains(&room_version_id) =>
            {
                room_version_id
            }
            _ => {
                return Err(Error::BadServerResponse(
                    "Room version is not supported",
                ))
            }
        };
        let mut join_event_stub: CanonicalJsonObject = serde_json::from_str(
            make_join_response.event.get(),
        )
        .map_err(|_| {
            Error::BadServerResponse(
                "Invalid make_join event json received from server.",
            )
        })?;
        let join_authorized_via_users_server = join_event_stub
            .get("content")
            .map(|s| {
                s.as_object()?.get("join_authorised_via_users_server")?.as_str()
            })
            .and_then(|s| OwnedUserId::try_from(s.unwrap_or_default()).ok());
        // TODO: Is origin needed?
        join_event_stub.insert(
            "origin".to_owned(),
            CanonicalJsonValue::String(
                services().globals.server_name().as_str().to_owned(),
            ),
        );
        join_event_stub.insert(
            "origin_server_ts".to_owned(),
            CanonicalJsonValue::Integer(
                utils::millis_since_unix_epoch()
                    .try_into()
                    .expect("Timestamp is valid js_int value"),
            ),
        );
        join_event_stub.insert(
            "content".to_owned(),
            to_canonical_value(RoomMemberEventContent {
                membership: MembershipState::Join,
                displayname: services().users.displayname(sender_user)?,
                avatar_url: services().users.avatar_url(sender_user)?,
                is_direct: None,
                third_party_invite: None,
                blurhash: services().users.blurhash(sender_user)?,
                reason,
                join_authorized_via_users_server,
            })
            .expect("event is valid, we just created it"),
        );

        // We don't leave the event id in the pdu because that's only allowed in
        // v1 or v2 rooms
        join_event_stub.remove("event_id");

        // In order to create a compatible ref hash (EventID) the `hashes` field
        // needs to be present
        ruma::signatures::hash_and_sign_event(
            services().globals.server_name().as_str(),
            services().globals.keypair(),
            &mut join_event_stub,
            &room_version_id,
        )
        .expect("event is valid, we just created it");

        // Generate event id
        let event_id = format!(
            "${}",
            ruma::signatures::reference_hash(
                &join_event_stub,
                &room_version_id
            )
            .expect("ruma can calculate reference hashes")
        );
        let event_id = <&EventId>::try_from(event_id.as_str())
            .expect("ruma's reference hashes are valid event ids");

        // Add event_id back
        join_event_stub.insert(
            "event_id".to_owned(),
            CanonicalJsonValue::String(event_id.as_str().to_owned()),
        );

        // It has enough fields to be called a proper event now
        let join_event = join_event_stub;

        let send_join_response = services()
            .sending
            .send_federation_request(
                &remote_server,
                federation::membership::create_join_event::v2::Request {
                    room_id: room_id.to_owned(),
                    event_id: event_id.to_owned(),
                    pdu: PduEvent::convert_to_outgoing_federation_event(
                        join_event.clone(),
                    ),
                    omit_members: false,
                },
            )
            .await?;

        let Some(signed_raw) = send_join_response.room_state.event else {
            return Err(error);
        };

        let Ok((signed_event_id, signed_value)) =
            gen_event_id_canonical_json(&signed_raw, &room_version_id)
        else {
            // Event could not be converted to canonical json
            return Err(Error::BadRequest(
                ErrorKind::InvalidParam,
                "Could not convert event to canonical json.",
            ));
        };

        if signed_event_id != event_id {
            return Err(Error::BadRequest(
                ErrorKind::InvalidParam,
                "Server sent event with wrong event id",
            ));
        }

        drop(room_token);
        let pub_key_map = RwLock::new(BTreeMap::new());
        services()
            .rooms
            .event_handler
            .handle_incoming_pdu(
                &remote_server,
                &signed_event_id,
                room_id,
                signed_value,
                true,
                &pub_key_map,
            )
            .await?;
    } else {
        info!("Joining over federation.");

        let (make_join_response, remote_server) =
            make_join_request(sender_user, room_id, servers).await?;

        info!("make_join finished");

        let room_version_id = match make_join_response.room_version {
            Some(room_version)
                if services()
                    .globals
                    .supported_room_versions()
                    .contains(&room_version) =>
            {
                room_version
            }
            _ => {
                return Err(Error::BadServerResponse(
                    "Room version is not supported",
                ))
            }
        };

        let mut join_event_stub: CanonicalJsonObject = serde_json::from_str(
            make_join_response.event.get(),
        )
        .map_err(|_| {
            Error::BadServerResponse(
                "Invalid make_join event json received from server.",
            )
        })?;

        let join_authorized_via_users_server = join_event_stub
            .get("content")
            .map(|s| {
                s.as_object()?.get("join_authorised_via_users_server")?.as_str()
            })
            .and_then(|s| OwnedUserId::try_from(s.unwrap_or_default()).ok());

        // TODO: Is origin needed?
        join_event_stub.insert(
            "origin".to_owned(),
            CanonicalJsonValue::String(
                services().globals.server_name().as_str().to_owned(),
            ),
        );
        join_event_stub.insert(
            "origin_server_ts".to_owned(),
            CanonicalJsonValue::Integer(
                utils::millis_since_unix_epoch()
                    .try_into()
                    .expect("Timestamp is valid js_int value"),
            ),
        );
        join_event_stub.insert(
            "content".to_owned(),
            to_canonical_value(RoomMemberEventContent {
                membership: MembershipState::Join,
                displayname: services().users.displayname(sender_user)?,
                avatar_url: services().users.avatar_url(sender_user)?,
                is_direct: None,
                third_party_invite: None,
                blurhash: services().users.blurhash(sender_user)?,
                reason,
                join_authorized_via_users_server,
            })
            .expect("event is valid, we just created it"),
        );

        // We don't leave the event id in the pdu because that's only allowed in
        // v1 or v2 rooms
        join_event_stub.remove("event_id");

        // In order to create a compatible ref hash (EventID) the `hashes` field
        // needs to be present
        ruma::signatures::hash_and_sign_event(
            services().globals.server_name().as_str(),
            services().globals.keypair(),
            &mut join_event_stub,
            &room_version_id,
        )
        .expect("event is valid, we just created it");

        // Generate event id
        let event_id = format!(
            "${}",
            ruma::signatures::reference_hash(
                &join_event_stub,
                &room_version_id
            )
            .expect("ruma can calculate reference hashes")
        );
        let event_id = <&EventId>::try_from(event_id.as_str())
            .expect("ruma's reference hashes are valid event ids");

        // Add event_id back
        join_event_stub.insert(
            "event_id".to_owned(),
            CanonicalJsonValue::String(event_id.as_str().to_owned()),
        );

        // It has enough fields to be called a proper event now
        let mut join_event = join_event_stub;

        info!(server = %remote_server, "Asking other server for send_join");
        let send_join_response = services()
            .sending
            .send_federation_request(
                &remote_server,
                federation::membership::create_join_event::v2::Request {
                    room_id: room_id.to_owned(),
                    event_id: event_id.to_owned(),
                    pdu: PduEvent::convert_to_outgoing_federation_event(
                        join_event.clone(),
                    ),
                    omit_members: false,
                },
            )
            .await?;

        info!("send_join finished");

        if let Some(signed_raw) = &send_join_response.room_state.event {
            info!(
                "There is a signed event. This room is probably using \
                 restricted joins. Adding signature to our event"
            );
            let Ok((signed_event_id, signed_value)) =
                gen_event_id_canonical_json(signed_raw, &room_version_id)
            else {
                // Event could not be converted to canonical json
                return Err(Error::BadRequest(
                    ErrorKind::InvalidParam,
                    "Could not convert event to canonical json.",
                ));
            };

            if signed_event_id != event_id {
                return Err(Error::BadRequest(
                    ErrorKind::InvalidParam,
                    "Server sent event with wrong event id",
                ));
            }

            match signed_value["signatures"]
                .as_object()
                .ok_or(Error::BadRequest(
                    ErrorKind::InvalidParam,
                    "Server sent invalid signatures type",
                ))
                .and_then(|e| {
                    e.get(remote_server.as_str()).ok_or(Error::BadRequest(
                        ErrorKind::InvalidParam,
                        "Server did not send its signature",
                    ))
                }) {
                Ok(signature) => {
                    join_event
                        .get_mut("signatures")
                        .expect("we created a valid pdu")
                        .as_object_mut()
                        .expect("we created a valid pdu")
                        .insert(remote_server.to_string(), signature.clone());
                }
                Err(error) => {
                    warn!(
                        %error,
                        server = %remote_server,
                        event = ?signed_value,
                        "Other server sent invalid signature in sendjoin \
                        signatures for event",
                    );
                }
            }
        }

        services().rooms.short.get_or_create_shortroomid(room_id)?;

        info!("Parsing join event");
        let parsed_join_pdu =
            PduEvent::from_id_val(event_id, join_event.clone()).map_err(
                |_| Error::BadServerResponse("Invalid join event PDU."),
            )?;

        let mut state = HashMap::new();
        let pub_key_map = RwLock::new(BTreeMap::new());

        info!("Fetching join signing keys");
        services()
            .rooms
            .event_handler
            .fetch_join_signing_keys(
                &send_join_response,
                &room_version_id,
                &pub_key_map,
            )
            .await?;

        info!("Going through send_join response room_state");
        for result in send_join_response.room_state.state.iter().map(|pdu| {
            validate_and_add_event_id(pdu, &room_version_id, &pub_key_map)
        }) {
            let Ok((event_id, value)) = result.await else {
                continue;
            };

            let pdu = PduEvent::from_id_val(&event_id, value.clone()).map_err(
                |error| {
                    warn!(
                        %error,
                        object = ?value,
                        "Invalid PDU in send_join response",
                    );
                    Error::BadServerResponse(
                        "Invalid PDU in send_join response.",
                    )
                },
            )?;

            services().rooms.outlier.add_pdu_outlier(&event_id, &value)?;
            if let Some(state_key) = &pdu.state_key {
                let shortstatekey =
                    services().rooms.short.get_or_create_shortstatekey(
                        &pdu.kind.to_string().into(),
                        state_key,
                    )?;
                state.insert(shortstatekey, pdu.event_id.clone());
            }
        }

        info!("Going through send_join response auth_chain");
        for result in
            send_join_response.room_state.auth_chain.iter().map(|pdu| {
                validate_and_add_event_id(pdu, &room_version_id, &pub_key_map)
            })
        {
            let Ok((event_id, value)) = result.await else {
                continue;
            };

            services().rooms.outlier.add_pdu_outlier(&event_id, &value)?;
        }

        info!("Running send_join auth check");
        let authenticated = state_res::event_auth::auth_check(
            &state_res::RoomVersion::new(&room_version_id)
                .expect("room version is supported"),
            &parsed_join_pdu,
            // TODO: third party invite
            None::<PduEvent>,
            |k, s| {
                services()
                    .rooms
                    .timeline
                    .get_pdu(
                        state.get(
                            &services()
                                .rooms
                                .short
                                .get_or_create_shortstatekey(
                                    &k.to_string().into(),
                                    s,
                                )
                                .ok()?,
                        )?,
                    )
                    .ok()?
            },
        )
        .map_err(|error| {
            warn!(%error, "Auth check failed");
            Error::BadRequest(ErrorKind::InvalidParam, "Auth check failed")
        })?;

        if !authenticated {
            return Err(Error::BadRequest(
                ErrorKind::InvalidParam,
                "Auth check failed",
            ));
        }

        info!("Saving state from send_join");
        let (statehash_before_join, new, removed) =
            services().rooms.state_compressor.save_state(
                room_id,
                Arc::new(
                    state
                        .into_iter()
                        .map(|(k, id)| {
                            services()
                                .rooms
                                .state_compressor
                                .compress_state_event(k, &id)
                        })
                        .collect::<Result<_>>()?,
                ),
            )?;

        services()
            .rooms
            .state
            .force_state(&room_token, statehash_before_join, new, removed)
            .await?;

        info!("Updating joined counts for new room");
        services().rooms.state_cache.update_joined_count(room_id)?;

        // We append to state before appending the pdu, so we don't have a
        // moment in time with the pdu without it's state. This is okay
        // because append_pdu can't fail.
        let statehash_after_join =
            services().rooms.state.append_to_state(&parsed_join_pdu)?;

        info!("Appending new room join event");
        services()
            .rooms
            .timeline
            .append_pdu(
                &parsed_join_pdu,
                join_event,
                vec![(*parsed_join_pdu.event_id).to_owned()],
                &room_token,
            )
            .await?;

        info!("Setting final room state for new room");
        // We set the room state after inserting the pdu, so that we never have
        // a moment in time where events in the current room state do
        // not exist
        services()
            .rooms
            .state
            .set_room_state(&room_token, statehash_after_join)?;
    }

    Ok(join_room_by_id::v3::Response::new(room_id.to_owned()))
}

async fn make_join_request(
    sender_user: &UserId,
    room_id: &RoomId,
    servers: &[OwnedServerName],
) -> Result<(
    federation::membership::prepare_join_event::v1::Response,
    OwnedServerName,
)> {
    let mut make_join_response_and_server = Err(Error::BadServerResponse(
        "No server available to assist in joining.",
    ));

    for remote_server in servers {
        if remote_server == services().globals.server_name() {
            continue;
        }
        info!(server = %remote_server, "Asking other server for make_join");
        let make_join_response = services()
            .sending
            .send_federation_request(
                remote_server,
                federation::membership::prepare_join_event::v1::Request {
                    room_id: room_id.to_owned(),
                    user_id: sender_user.to_owned(),
                    ver: services().globals.supported_room_versions(),
                },
            )
            .await;

        make_join_response_and_server =
            make_join_response.map(|r| (r, remote_server.clone()));

        if make_join_response_and_server.is_ok() {
            break;
        }
    }

    make_join_response_and_server
}

async fn validate_and_add_event_id(
    pdu: &RawJsonValue,
    room_version: &RoomVersionId,
    pub_key_map: &RwLock<BTreeMap<String, SigningKeys>>,
) -> Result<(OwnedEventId, CanonicalJsonObject)> {
    let mut value: CanonicalJsonObject = serde_json::from_str(pdu.get())
        .map_err(|error| {
            error!(%error, object = ?pdu, "Invalid PDU in server response");
            Error::BadServerResponse("Invalid PDU in server response")
        })?;
    let event_id = EventId::parse(format!(
        "${}",
        ruma::signatures::reference_hash(&value, room_version)
            .expect("ruma can calculate reference hashes")
    ))
    .expect("ruma's reference hashes are valid event ids");

    let back_off = |id| async {
        match services().globals.bad_event_ratelimiter.write().await.entry(id) {
            Entry::Vacant(e) => {
                e.insert((Instant::now(), 1));
            }
            Entry::Occupied(mut e) => {
                *e.get_mut() = (Instant::now(), e.get().1 + 1);
            }
        }
    };

    if let Some((time, tries)) =
        services().globals.bad_event_ratelimiter.read().await.get(&event_id)
    {
        // Exponential backoff
        let mut min_elapsed_duration =
            Duration::from_secs(30) * (*tries) * (*tries);
        if min_elapsed_duration > Duration::from_secs(60 * 60 * 24) {
            min_elapsed_duration = Duration::from_secs(60 * 60 * 24);
        }

        if time.elapsed() < min_elapsed_duration {
            debug!(%event_id, "Backing off from event");
            return Err(Error::BadServerResponse(
                "bad event, still backing off",
            ));
        }
    }

    let origin_server_ts = value.get("origin_server_ts").ok_or_else(|| {
        error!("Invalid PDU, no origin_server_ts field");
        Error::BadRequest(
            ErrorKind::MissingParam,
            "Invalid PDU, no origin_server_ts field",
        )
    })?;

    let origin_server_ts: MilliSecondsSinceUnixEpoch = {
        let ts = origin_server_ts.as_integer().ok_or_else(|| {
            Error::BadRequest(
                ErrorKind::InvalidParam,
                "origin_server_ts must be an integer",
            )
        })?;

        MilliSecondsSinceUnixEpoch(i64::from(ts).try_into().map_err(|_| {
            Error::BadRequest(
                ErrorKind::InvalidParam,
                "Time must be after the unix epoch",
            )
        })?)
    };

    let unfiltered_keys = (*pub_key_map.read().await).clone();

    let keys = services().globals.filter_keys_server_map(
        unfiltered_keys,
        origin_server_ts,
        room_version,
    );

    if let Err(error) =
        ruma::signatures::verify_event(&keys, &value, room_version)
    {
        warn!(
            %event_id,
            %error,
            ?pdu,
            "Event failed verification",
        );
        back_off(event_id).await;
        return Err(Error::BadServerResponse("Event failed verification."));
    }

    value.insert(
        "event_id".to_owned(),
        CanonicalJsonValue::String(event_id.as_str().to_owned()),
    );

    Ok((event_id, value))
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn invite_helper(
    sender_user: &UserId,
    user_id: &UserId,
    room_id: &RoomId,
    reason: Option<String>,
    is_direct: bool,
) -> Result<()> {
    if user_id.server_name() != services().globals.server_name() {
        let (pdu, pdu_json, invite_room_state) = {
            let room_token = services()
                .globals
                .roomid_mutex_state
                .lock_key(room_id.to_owned())
                .await;

            let content = to_raw_value(&RoomMemberEventContent {
                avatar_url: None,
                displayname: None,
                is_direct: Some(is_direct),
                membership: MembershipState::Invite,
                third_party_invite: None,
                blurhash: None,
                reason,
                join_authorized_via_users_server: None,
            })
            .expect("member event is valid value");

            let (pdu, pdu_json) =
                services().rooms.timeline.create_hash_and_sign_event(
                    PduBuilder {
                        event_type: TimelineEventType::RoomMember,
                        content,
                        unsigned: None,
                        state_key: Some(user_id.to_string()),
                        redacts: None,
                    },
                    sender_user,
                    &room_token,
                )?;

            let invite_room_state =
                services().rooms.state.calculate_invite_state(&pdu)?;

            drop(room_token);

            (pdu, pdu_json, invite_room_state)
        };

        let room_version_id =
            services().rooms.state.get_room_version(room_id)?;

        let response = services()
            .sending
            .send_federation_request(
                user_id.server_name(),
                create_invite::v2::Request {
                    room_id: room_id.to_owned(),
                    event_id: (*pdu.event_id).to_owned(),
                    room_version: room_version_id.clone(),
                    event: PduEvent::convert_to_outgoing_federation_event(
                        pdu_json.clone(),
                    ),
                    invite_room_state,
                },
            )
            .await?;

        let pub_key_map = RwLock::new(BTreeMap::new());

        // We do not add the event_id field to the pdu here because of signature
        // and hashes checks
        let Ok((event_id, value)) =
            gen_event_id_canonical_json(&response.event, &room_version_id)
        else {
            // Event could not be converted to canonical json
            return Err(Error::BadRequest(
                ErrorKind::InvalidParam,
                "Could not convert event to canonical json.",
            ));
        };

        if *pdu.event_id != *event_id {
            warn!(
                server = %user_id.server_name(),
                our_object = ?pdu_json,
                their_object = ?value,
                "Other server changed invite event, that's not allowed in the \
                 spec",
            );
        }

        let origin: OwnedServerName = serde_json::from_value(
            serde_json::to_value(value.get("origin").ok_or(
                Error::BadRequest(
                    ErrorKind::InvalidParam,
                    "Event needs an origin field.",
                ),
            )?)
            .expect("CanonicalJson is valid json value"),
        )
        .map_err(|_| {
            Error::BadRequest(
                ErrorKind::InvalidParam,
                "Origin field is invalid.",
            )
        })?;

        let pdu_id: Vec<u8> = services()
            .rooms
            .event_handler
            .handle_incoming_pdu(
                &origin,
                &event_id,
                room_id,
                value,
                true,
                &pub_key_map,
            )
            .await?
            .ok_or(Error::BadRequest(
                ErrorKind::InvalidParam,
                "Could not accept incoming PDU as timeline event.",
            ))?;

        // Bind to variable because of lifetimes
        let servers = services()
            .rooms
            .state_cache
            .room_servers(room_id)
            .filter_map(Result::ok)
            .filter(|server| &**server != services().globals.server_name());

        services().sending.send_pdu(servers, &pdu_id)?;

        return Ok(());
    }

    if !services().rooms.state_cache.is_joined(sender_user, room_id)? {
        return Err(Error::BadRequest(
            ErrorKind::forbidden(),
            "You don't have permission to view this room.",
        ));
    }

    let room_token = services()
        .globals
        .roomid_mutex_state
        .lock_key(room_id.to_owned())
        .await;

    services()
        .rooms
        .timeline
        .build_and_append_pdu(
            PduBuilder {
                event_type: TimelineEventType::RoomMember,
                content: to_raw_value(&RoomMemberEventContent {
                    membership: MembershipState::Invite,
                    displayname: services().users.displayname(user_id)?,
                    avatar_url: services().users.avatar_url(user_id)?,
                    is_direct: Some(is_direct),
                    third_party_invite: None,
                    blurhash: services().users.blurhash(user_id)?,
                    reason,
                    join_authorized_via_users_server: None,
                })
                .expect("event is valid, we just created it"),
                unsigned: None,
                state_key: Some(user_id.to_string()),
                redacts: None,
            },
            sender_user,
            &room_token,
        )
        .await?;

    drop(room_token);

    Ok(())
}

// Make a user leave all their joined rooms
pub(crate) async fn leave_all_rooms(user_id: &UserId) -> Result<()> {
    let all_rooms = services()
        .rooms
        .state_cache
        .rooms_joined(user_id)
        .chain(
            services()
                .rooms
                .state_cache
                .rooms_invited(user_id)
                .map(|t| t.map(|(r, _)| r)),
        )
        .collect::<Vec<_>>();

    for room_id in all_rooms {
        let Ok(room_id) = room_id else {
            continue;
        };

        if let Err(error) = leave_room(user_id, &room_id, None).await {
            warn!(%user_id, %room_id, %error, "Failed to leave room");
        }
    }

    Ok(())
}

#[tracing::instrument(skip(reason))]
pub(crate) async fn leave_room(
    user_id: &UserId,
    room_id: &RoomId,
    reason: Option<String>,
) -> Result<()> {
    // Ask a remote server if we don't have this room
    if services()
        .rooms
        .state_cache
        .server_in_room(services().globals.server_name(), room_id)?
    {
        let room_token = services()
            .globals
            .roomid_mutex_state
            .lock_key(room_id.to_owned())
            .await;

        let member_event = services().rooms.state_accessor.room_state_get(
            room_id,
            &StateEventType::RoomMember,
            user_id.as_str(),
        )?;

        // Fix for broken rooms
        let member_event = match member_event {
            None => {
                error!("Trying to leave a room you are not a member of.");

                services().rooms.state_cache.update_membership(
                    room_id,
                    user_id,
                    MembershipState::Leave,
                    user_id,
                    None,
                    true,
                )?;
                return Ok(());
            }
            Some(e) => e,
        };

        let mut event: RoomMemberEventContent =
            serde_json::from_str(member_event.content.get()).map_err(|_| {
                Error::bad_database("Invalid member event in database.")
            })?;

        event.membership = MembershipState::Leave;
        event.reason = reason;
        event.join_authorized_via_users_server = None;

        services()
            .rooms
            .timeline
            .build_and_append_pdu(
                PduBuilder {
                    event_type: TimelineEventType::RoomMember,
                    content: to_raw_value(&event)
                        .expect("event is valid, we just created it"),
                    unsigned: None,
                    state_key: Some(user_id.to_string()),
                    redacts: None,
                },
                user_id,
                &room_token,
            )
            .await?;
    } else {
        if let Err(error) = remote_leave_room(user_id, room_id).await {
            warn!(%error, "Failed to leave room remotely");
            // Don't tell the client about this error
        }

        let last_state = services()
            .rooms
            .state_cache
            .invite_state(user_id, room_id)?
            .map_or_else(
                || services().rooms.state_cache.left_state(user_id, room_id),
                |s| Ok(Some(s)),
            )?;

        // We always drop the invite, we can't rely on other servers
        services().rooms.state_cache.update_membership(
            room_id,
            user_id,
            MembershipState::Leave,
            user_id,
            last_state,
            true,
        )?;
    }

    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn remote_leave_room(user_id: &UserId, room_id: &RoomId) -> Result<()> {
    let mut make_leave_response_and_server = Err(Error::BadServerResponse(
        "No server available to assist in leaving.",
    ));

    let servers: HashSet<_> = find_participating_servers(user_id, room_id)?
        .ok_or(Error::BadRequest(ErrorKind::BadState, "User is not invited."))
        .map(Iterator::collect)?;

    for remote_server in servers {
        let make_leave_response = services()
            .sending
            .send_federation_request(
                &remote_server,
                federation::membership::prepare_leave_event::v1::Request {
                    room_id: room_id.to_owned(),
                    user_id: user_id.to_owned(),
                },
            )
            .await;

        make_leave_response_and_server =
            make_leave_response.map(|r| (r, remote_server));

        if make_leave_response_and_server.is_ok() {
            break;
        }
    }

    let (make_leave_response, remote_server) = make_leave_response_and_server?;

    let room_version_id = match make_leave_response.room_version {
        Some(version)
            if services()
                .globals
                .supported_room_versions()
                .contains(&version) =>
        {
            version
        }
        _ => {
            return Err(Error::BadServerResponse(
                "Room version is not supported",
            ))
        }
    };

    let mut leave_event_stub = serde_json::from_str::<CanonicalJsonObject>(
        make_leave_response.event.get(),
    )
    .map_err(|_| {
        Error::BadServerResponse(
            "Invalid make_leave event json received from server.",
        )
    })?;

    // TODO: Is origin needed?
    leave_event_stub.insert(
        "origin".to_owned(),
        CanonicalJsonValue::String(
            services().globals.server_name().as_str().to_owned(),
        ),
    );
    leave_event_stub.insert(
        "origin_server_ts".to_owned(),
        CanonicalJsonValue::Integer(
            utils::millis_since_unix_epoch()
                .try_into()
                .expect("Timestamp is valid js_int value"),
        ),
    );
    // We don't leave the event id in the pdu because that's only allowed in v1
    // or v2 rooms
    leave_event_stub.remove("event_id");

    // In order to create a compatible ref hash (EventID) the `hashes` field
    // needs to be present
    ruma::signatures::hash_and_sign_event(
        services().globals.server_name().as_str(),
        services().globals.keypair(),
        &mut leave_event_stub,
        &room_version_id,
    )
    .expect("event is valid, we just created it");

    // Generate event id
    let event_id = EventId::parse(format!(
        "${}",
        ruma::signatures::reference_hash(&leave_event_stub, &room_version_id)
            .expect("ruma can calculate reference hashes")
    ))
    .expect("ruma's reference hashes are valid event ids");

    // Add event_id back
    leave_event_stub.insert(
        "event_id".to_owned(),
        CanonicalJsonValue::String(event_id.as_str().to_owned()),
    );

    // It has enough fields to be called a proper event now
    let leave_event = leave_event_stub;

    services()
        .sending
        .send_federation_request(
            &remote_server,
            federation::membership::create_leave_event::v2::Request {
                room_id: room_id.to_owned(),
                event_id,
                pdu: PduEvent::convert_to_outgoing_federation_event(
                    leave_event.clone(),
                ),
            },
        )
        .await?;

    Ok(())
}
