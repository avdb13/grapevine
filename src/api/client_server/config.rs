use ruma::{
    api::client::{
        config::{
            get_global_account_data, get_room_account_data,
            set_global_account_data, set_room_account_data,
        },
        error::ErrorKind,
    },
    events::{
        AnyGlobalAccountDataEventContent, AnyRoomAccountDataEventContent,
    },
    serde::Raw,
};
use serde::Deserialize;
use serde_json::{json, value::RawValue as RawJsonValue};

use crate::{services, Ar, Error, Ra, Result};

/// # `PUT /_matrix/client/r0/user/{userId}/account_data/{type}`
///
/// Sets some account data for the sender user.
pub(crate) async fn set_global_account_data_route(
    body: Ar<set_global_account_data::v3::Request>,
) -> Result<Ra<set_global_account_data::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let data: serde_json::Value = serde_json::from_str(body.data.json().get())
        .map_err(|_| {
            Error::BadRequest(ErrorKind::BadJson, "Data is invalid.")
        })?;

    let event_type = body.event_type.to_string();

    services().account_data.update(
        None,
        sender_user,
        event_type.clone().into(),
        &json!({
            "type": event_type,
            "content": data,
        }),
    )?;

    Ok(Ra(set_global_account_data::v3::Response {}))
}

/// # `PUT /_matrix/client/r0/user/{userId}/rooms/{roomId}/account_data/{type}`
///
/// Sets some room account data for the sender user.
pub(crate) async fn set_room_account_data_route(
    body: Ar<set_room_account_data::v3::Request>,
) -> Result<Ra<set_room_account_data::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let data: serde_json::Value = serde_json::from_str(body.data.json().get())
        .map_err(|_| {
            Error::BadRequest(ErrorKind::BadJson, "Data is invalid.")
        })?;

    let event_type = body.event_type.to_string();

    services().account_data.update(
        Some(&body.room_id),
        sender_user,
        event_type.clone().into(),
        &json!({
            "type": event_type,
            "content": data,
        }),
    )?;

    Ok(Ra(set_room_account_data::v3::Response {}))
}

/// # `GET /_matrix/client/r0/user/{userId}/account_data/{type}`
///
/// Gets some account data for the sender user.
pub(crate) async fn get_global_account_data_route(
    body: Ar<get_global_account_data::v3::Request>,
) -> Result<Ra<get_global_account_data::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let event: Box<RawJsonValue> = services()
        .account_data
        .get(None, sender_user, body.event_type.to_string().into())?
        .ok_or(Error::BadRequest(ErrorKind::NotFound, "Data not found."))?;

    let account_data =
        serde_json::from_str::<ExtractGlobalEventContent>(event.get())
            .map_err(|_| {
                Error::bad_database("Invalid account data event in db.")
            })?
            .content;

    Ok(Ra(get_global_account_data::v3::Response {
        account_data,
    }))
}

/// # `GET /_matrix/client/r0/user/{userId}/rooms/{roomId}/account_data/{type}`
///
/// Gets some room account data for the sender user.
pub(crate) async fn get_room_account_data_route(
    body: Ar<get_room_account_data::v3::Request>,
) -> Result<Ra<get_room_account_data::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let event: Box<RawJsonValue> = services()
        .account_data
        .get(Some(&body.room_id), sender_user, body.event_type.clone())?
        .ok_or(Error::BadRequest(ErrorKind::NotFound, "Data not found."))?;

    let account_data =
        serde_json::from_str::<ExtractRoomEventContent>(event.get())
            .map_err(|_| {
                Error::bad_database("Invalid account data event in db.")
            })?
            .content;

    Ok(Ra(get_room_account_data::v3::Response {
        account_data,
    }))
}

#[derive(Deserialize)]
struct ExtractRoomEventContent {
    content: Raw<AnyRoomAccountDataEventContent>,
}

#[derive(Deserialize)]
struct ExtractGlobalEventContent {
    content: Raw<AnyGlobalAccountDataEventContent>,
}
