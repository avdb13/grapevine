use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use ruma::{
    api::client::error::ErrorKind,
    canonical_json::to_canonical_value,
    events::{
        room::{
            join_rules::{AllowRule, JoinRule, RoomJoinRulesEventContent},
            member::RoomMemberEventContent,
        },
        StateEventType,
    },
    CanonicalJsonObject, CanonicalJsonValue, EventId, OwnedEventId,
    OwnedServerName, RoomId, RoomVersionId, ServerName, UserId,
};
use serde_json::{value::RawValue as RawJsonValue, Value};
use tracing::{info, warn};

use crate::{service::pdu, services, utils, Error, PduEvent, Result};

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

pub(crate) fn find_join_conditions(
    sender_user: &UserId,
    room_id: &RoomId,
) -> Result<Option<Vec<AllowRule>>> {
    let Some(event) = services().rooms.state_accessor.room_state_get(
        room_id,
        &StateEventType::RoomJoinRules,
        "",
    )?
    else {
        return Ok(None);
    };

    let content: RoomJoinRulesEventContent =
        serde_json::from_str(event.content.get()).map_err(|e| {
            warn!("Invalid join rules event: {}", e);

            Error::bad_database("Invalid join rules event in db.")
        })?;

    let (JoinRule::Restricted(r) | JoinRule::KnockRestricted(r)) =
        content.join_rule
    else {
        return Ok(None);
    };
    Ok(Some(
        r.allow
            .into_iter()
            .filter(|r| match r {
                AllowRule::RoomMembership(m) => services()
                    .rooms
                    .state_cache
                    .is_joined(sender_user, &m.room_id)
                    .unwrap_or(false),
                _ => false,
            })
            .collect(),
    ))
}

pub(crate) fn prepare_make_join_stub(
    stub: &RawJsonValue,
    room_version_id: &RoomVersionId,
    content: RoomMemberEventContent,
) -> Result<(OwnedEventId, CanonicalJsonObject)> {
    let mut stub: CanonicalJsonObject = serde_json::from_str(stub.get())
        .map_err(|_| {
            Error::BadServerResponse(
                "Invalid make_join event json received from server.",
            )
        })?;

    let _join_authorized_via_users_server =
        stub.get("content").and_then(|content| {
            let auth_user =
                content.as_object()?.get("join_authorised_via_users_server")?;

            auth_user.as_str().and_then(|s| UserId::parse(s).ok())
        });

    // Keeping the PDU's `event_id` field is only allowed for in version `V1`
    // and `V2`
    stub.remove("event_id");

    let origin = format!("{}", services().globals.server_name());
    stub.insert("origin".to_owned(), CanonicalJsonValue::String(origin));

    let origin_server_ts =
        utils::millis_since_unix_epoch().try_into().expect("integer overflow");
    stub.insert(
        "origin_server_ts".to_owned(),
        CanonicalJsonValue::Integer(origin_server_ts),
    );

    stub.insert(
        "content".to_owned(),
        to_canonical_value(content)
            .expect("event is valid, we just created it"),
    );

    // We hash and sign before inserting "event_id" in order to create a
    // compatible reference hash
    ruma::signatures::hash_and_sign_event(
        services().globals.server_name().as_str(),
        services().globals.keypair(),
        &mut stub,
        room_version_id,
    )
    .expect("event is valid, we just created it");

    // Generate event id
    let reference_hash =
        ruma::signatures::reference_hash(&stub, room_version_id)
            .expect("ruma can calculate reference hashes");

    // TODO: do we need this?
    let event_id = EventId::parse(format!("${reference_hash}"))
        .expect("ruma's reference hashes are valid event ids");

    // Add event_id back
    stub.insert(
        "event_id".to_owned(),
        CanonicalJsonValue::String(event_id.to_string()),
    );

    // It has enough fields to be called a proper event now
    Ok((event_id.clone(), stub))
}

pub(crate) fn validate_send_join_signature(
    event_id: &EventId,
    event: &RawJsonValue,
    room_version_id: &RoomVersionId,
    remote_server: &ServerName,
) -> Result<Option<CanonicalJsonValue>> {
    info!(
        "There is a signed event. This room is probably using restricted \
         joins. Adding signature to our event"
    );

    let (signed_event_id, signed_event) =
        pdu::gen_event_id_canonical_json(event, room_version_id).map_err(
            |_| {
                Error::BadRequest(
                    ErrorKind::InvalidParam,
                    "Could not convert event to canonical json.",
                )
            },
        )?;

    if signed_event_id != event_id {
        return Err(Error::BadRequest(
            ErrorKind::InvalidParam,
            "Server sent event with wrong event id",
        ));
    }

    let Some(signatures) =
        signed_event.get("signatures").and_then(|value| value.as_object())
    else {
        warn!(
            server = %remote_server,
            event = ?signed_event,
            "server sent signed event without any signatures in send_join response",
        );

        return Ok(None);
    };

    let Some(signature) = signatures.get(remote_server.as_str()) else {
        warn!(
            server = %remote_server,
            event = ?signed_event,
            "server sent signed event with an invalid signature in send_join response",
        );

        return Ok(None);
    };

    Ok(Some(signature.to_owned()))
}

pub(crate) fn build_state_snapshot<'pdus, I>(
    mut pdus: I,
) -> Result<HashMap<u64, Arc<EventId>>>
where
    I: Iterator<
        Item = &'pdus (OwnedEventId, BTreeMap<String, CanonicalJsonValue>),
    >,
{
    pdus.try_fold(HashMap::new(), |snapshot, (event_id, value)| {
        match PduEvent::from_id_val(event_id, value.clone()) {
            Err(error) => {
                warn!(
                    %error,
                    object = ?value,
                    "Invalid PDU in send_join response",
                );

                Err(Error::BadServerResponse(
                    "Invalid PDU in send_join response.",
                ))
            }
            Ok(pdu) => {
                let Some(state_key) = pdu.state_key.as_ref() else {
                    return Ok(snapshot);
                };

                let result =
                    services().rooms.short.get_or_create_shortstatekey(
                        &StateEventType::from(pdu.kind.to_string()),
                        state_key,
                    );

                result.map(|shortstatekey| {
                    let tail =
                        std::iter::once((shortstatekey, pdu.event_id.clone()));

                    snapshot.into_iter().chain(tail).collect()
                })
            }
        }
    })
}
