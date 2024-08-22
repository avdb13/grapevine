use std::{
    collections::{hash_map, BTreeMap, HashMap, HashSet},
    time::{Duration, Instant},
};

use futures_util::{stream::FuturesUnordered, StreamExt};
use ruma::{
    api::{
        client::{
            error::ErrorKind,
            keys::{
                claim_keys, get_key_changes, get_keys, upload_keys,
                upload_signatures, upload_signing_keys,
            },
            uiaa::{AuthFlow, AuthType, UiaaInfo},
        },
        federation,
    },
    encryption::CrossSigningKey,
    serde::Raw,
    DeviceKeyAlgorithm, OwnedDeviceId, OwnedDeviceKeyId, OwnedUserId, UserId,
};
use serde_json::json;
use tracing::debug;

use super::SESSION_ID_LENGTH;
use crate::{services, utils, Ar, Error, Ra, Result};

/// # `POST /_matrix/client/r0/keys/upload`
///
/// Publish end-to-end encryption keys for the sender device.
///
/// - Adds one time keys
/// - If there are no device keys yet: Adds device keys (TODO: merge with
///   existing keys?)
pub(crate) async fn upload_keys_route(
    body: Ar<upload_keys::v3::Request>,
) -> Result<Ra<upload_keys::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");
    let sender_device =
        body.sender_device.as_ref().expect("user is authenticated");

    for (key_key, key_value) in &body.one_time_keys {
        services().users.add_one_time_key(
            sender_user,
            sender_device,
            key_key,
            key_value,
        )?;
    }

    if let Some(device_keys) = &body.device_keys {
        // TODO: merge this and the existing event?
        // This check is needed to assure that signatures are kept
        if services()
            .users
            .get_device_keys(sender_user, sender_device)?
            .is_none()
        {
            services().users.add_device_keys(
                sender_user,
                sender_device,
                device_keys,
            )?;
        }
    }

    Ok(Ra(upload_keys::v3::Response {
        one_time_key_counts: services()
            .users
            .count_one_time_keys(sender_user, sender_device)?,
    }))
}

/// # `POST /_matrix/client/r0/keys/query`
///
/// Get end-to-end encryption keys for the given users.
///
/// - Always fetches users from other servers over federation
/// - Gets master keys, self-signing keys, user signing keys and device keys.
/// - The master and self-signing keys contain signatures that the user is
///   allowed to see
pub(crate) async fn get_keys_route(
    body: Ar<get_keys::v3::Request>,
) -> Result<Ra<get_keys::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let response = get_keys_helper(Some(sender_user), &body.device_keys, |u| {
        u == sender_user
    })
    .await?;

    Ok(Ra(response))
}

/// # `POST /_matrix/client/r0/keys/claim`
///
/// Claims one-time keys
pub(crate) async fn claim_keys_route(
    body: Ar<claim_keys::v3::Request>,
) -> Result<Ra<claim_keys::v3::Response>> {
    let response = claim_keys_helper(&body.one_time_keys).await?;

    Ok(Ra(response))
}

/// # `POST /_matrix/client/r0/keys/device_signing/upload`
///
/// Uploads end-to-end key information for the sender user.
///
/// Requires UIAA by default, unless one the following conditions are met:
/// - The user has no existing cross-signing master key.
/// - An existing cross-signing master key exactly matches the provided
///   cross-signing master key, and additionally provided keys also match
///   previously stored keys.
pub(crate) async fn upload_signing_keys_route(
    body: Ar<upload_signing_keys::v3::Request>,
) -> Result<Ra<upload_signing_keys::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");
    let sender_device =
        body.sender_device.as_ref().expect("user is authenticated");

    // UIAA
    let mut uiaainfo = UiaaInfo {
        flows: vec![AuthFlow {
            stages: vec![AuthType::Password],
        }],
        completed: Vec::new(),
        params: Box::default(),
        session: None,
        auth_error: None,
    };

    if let Ok(true) = upload_should_bypass_uia(sender_user, &body) {
        // UIA bypass
    } else if let Some(auth) = &body.auth {
        let (worked, uiaainfo) = services().uiaa.try_auth(
            sender_user,
            sender_device,
            auth,
            &uiaainfo,
        )?;
        if !worked {
            return Err(Error::Uiaa(uiaainfo));
        }
    // Success!
    } else if let Some(json) = body.json_body {
        uiaainfo.session = Some(utils::random_string(SESSION_ID_LENGTH));
        services().uiaa.create(sender_user, sender_device, &uiaainfo, &json)?;
        return Err(Error::Uiaa(uiaainfo));
    } else {
        return Err(Error::BadRequest(ErrorKind::NotJson, "Not json."));
    }

    if let Some(master_key) = &body.master_key {
        services().users.add_cross_signing_keys(
            sender_user,
            master_key,
            &body.self_signing_key,
            &body.user_signing_key,
            // notify so that other users see the new keys
            true,
        )?;
    }

    Ok(Ra(upload_signing_keys::v3::Response {}))
}

/// # `POST /_matrix/client/r0/keys/signatures/upload`
///
/// Uploads end-to-end key signatures from the sender user.
pub(crate) async fn upload_signatures_route(
    body: Ar<upload_signatures::v3::Request>,
) -> Result<Ra<upload_signatures::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    for (user_id, keys) in &body.signed_keys {
        for (key_id, key) in keys {
            let key = serde_json::to_value(key).map_err(|_| {
                Error::BadRequest(ErrorKind::InvalidParam, "Invalid key JSON")
            })?;

            for signature in key
                .get("signatures")
                .ok_or(Error::BadRequest(
                    ErrorKind::InvalidParam,
                    "Missing signatures field.",
                ))?
                .get(sender_user.to_string())
                .ok_or(Error::BadRequest(
                    ErrorKind::InvalidParam,
                    "Invalid user in signatures field.",
                ))?
                .as_object()
                .ok_or(Error::BadRequest(
                    ErrorKind::InvalidParam,
                    "Invalid signature.",
                ))?
                .clone()
            {
                // Signature validation?
                let signature = (
                    signature.0,
                    signature
                        .1
                        .as_str()
                        .ok_or(Error::BadRequest(
                            ErrorKind::InvalidParam,
                            "Invalid signature value.",
                        ))?
                        .to_owned(),
                );
                services().users.sign_key(
                    user_id,
                    key_id,
                    signature,
                    sender_user,
                )?;
            }
        }
    }

    Ok(Ra(upload_signatures::v3::Response {
        // TODO: integrate
        failures: BTreeMap::new(),
    }))
}

/// # `POST /_matrix/client/r0/keys/changes`
///
/// Gets a list of users who have updated their device identity keys since the
/// previous sync token.
///
/// - TODO: left users
pub(crate) async fn get_key_changes_route(
    body: Ar<get_key_changes::v3::Request>,
) -> Result<Ra<get_key_changes::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let mut device_list_updates = HashSet::new();

    device_list_updates.extend(
        services()
            .users
            .keys_changed(
                sender_user.as_str(),
                body.from.parse().map_err(|_| {
                    Error::BadRequest(
                        ErrorKind::InvalidParam,
                        "Invalid `from`.",
                    )
                })?,
                Some(body.to.parse().map_err(|_| {
                    Error::BadRequest(ErrorKind::InvalidParam, "Invalid `to`.")
                })?),
            )
            .filter_map(Result::ok),
    );

    for room_id in services()
        .rooms
        .state_cache
        .rooms_joined(sender_user)
        .filter_map(Result::ok)
    {
        device_list_updates.extend(
            services()
                .users
                .keys_changed(
                    room_id.as_ref(),
                    body.from.parse().map_err(|_| {
                        Error::BadRequest(
                            ErrorKind::InvalidParam,
                            "Invalid `from`.",
                        )
                    })?,
                    Some(body.to.parse().map_err(|_| {
                        Error::BadRequest(
                            ErrorKind::InvalidParam,
                            "Invalid `to`.",
                        )
                    })?),
                )
                .filter_map(Result::ok),
        );
    }
    Ok(Ra(get_key_changes::v3::Response {
        changed: device_list_updates.into_iter().collect(),
        // TODO
        left: Vec::new(),
    }))
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn get_keys_helper<F: Fn(&UserId) -> bool>(
    sender_user: Option<&UserId>,
    device_keys_input: &BTreeMap<OwnedUserId, Vec<OwnedDeviceId>>,
    allowed_signatures: F,
) -> Result<get_keys::v3::Response> {
    let mut master_keys = BTreeMap::new();
    let mut self_signing_keys = BTreeMap::new();
    let mut user_signing_keys = BTreeMap::new();
    let mut device_keys = BTreeMap::new();

    let mut get_over_federation = HashMap::new();

    for (user_id, device_ids) in device_keys_input {
        let user_id: &UserId = user_id;

        if user_id.server_name() != services().globals.server_name() {
            get_over_federation
                .entry(user_id.server_name())
                .or_insert_with(Vec::new)
                .push((user_id, device_ids));
            continue;
        }

        if device_ids.is_empty() {
            let mut container = BTreeMap::new();
            for device_id in services().users.all_device_ids(user_id) {
                let device_id = device_id?;
                if let Some(mut keys) =
                    services().users.get_device_keys(user_id, &device_id)?
                {
                    let metadata = services()
                        .users
                        .get_device_metadata(user_id, &device_id)?
                        .ok_or_else(|| {
                            Error::bad_database(
                                "all_device_keys contained nonexistent device.",
                            )
                        })?;

                    add_unsigned_device_display_name(&mut keys, metadata)
                        .map_err(|_| {
                            Error::bad_database(
                                "invalid device keys in database",
                            )
                        })?;
                    container.insert(device_id, keys);
                }
            }
            device_keys.insert(user_id.to_owned(), container);
        } else {
            for device_id in device_ids {
                let mut container = BTreeMap::new();
                if let Some(mut keys) =
                    services().users.get_device_keys(user_id, device_id)?
                {
                    let metadata = services()
                        .users
                        .get_device_metadata(user_id, device_id)?
                        .ok_or(Error::BadRequest(
                            ErrorKind::InvalidParam,
                            "Tried to get keys for nonexistent device.",
                        ))?;

                    add_unsigned_device_display_name(&mut keys, metadata)
                        .map_err(|_| {
                            Error::bad_database(
                                "invalid device keys in database",
                            )
                        })?;
                    container.insert(device_id.to_owned(), keys);
                }
                device_keys.insert(user_id.to_owned(), container);
            }
        }

        if let Some(master_key) = services().users.get_master_key(
            sender_user,
            user_id,
            &allowed_signatures,
        )? {
            master_keys.insert(user_id.to_owned(), master_key);
        }
        if let Some(self_signing_key) = services().users.get_self_signing_key(
            sender_user,
            user_id,
            &allowed_signatures,
        )? {
            self_signing_keys.insert(user_id.to_owned(), self_signing_key);
        }
        if Some(user_id) == sender_user {
            if let Some(user_signing_key) =
                services().users.get_user_signing_key(user_id)?
            {
                user_signing_keys.insert(user_id.to_owned(), user_signing_key);
            }
        }
    }

    let mut failures = BTreeMap::new();

    let back_off = |id| async {
        match services().globals.bad_query_ratelimiter.write().await.entry(id) {
            hash_map::Entry::Vacant(e) => {
                e.insert((Instant::now(), 1));
            }
            hash_map::Entry::Occupied(mut e) => {
                *e.get_mut() = (Instant::now(), e.get().1 + 1);
            }
        }
    };

    let mut futures: FuturesUnordered<_> = get_over_federation
        .into_iter()
        .map(|(server, vec)| async move {
            if let Some((time, tries)) = services()
                .globals
                .bad_query_ratelimiter
                .read()
                .await
                .get(server)
            {
                // Exponential backoff
                let mut min_elapsed_duration =
                    Duration::from_secs(30) * (*tries) * (*tries);
                if min_elapsed_duration > Duration::from_secs(60 * 60 * 24) {
                    min_elapsed_duration = Duration::from_secs(60 * 60 * 24);
                }

                if let Some(remaining) =
                    min_elapsed_duration.checked_sub(time.elapsed())
                {
                    debug!(%server, %tries, ?remaining, "Backing off from server");
                    return (
                        server,
                        Err(Error::BadServerResponse(
                            "bad query, still backing off",
                        )),
                    );
                }
            }

            let mut device_keys_input_fed = BTreeMap::new();
            for (user_id, keys) in vec {
                device_keys_input_fed.insert(user_id.to_owned(), keys.clone());
            }
            // TODO: switch .and_then(|result| result) to .flatten() when stable
            // <https://github.com/rust-lang/rust/issues/70142>
            (
                server,
                tokio::time::timeout(
                    Duration::from_secs(25),
                    services().sending.send_federation_request(
                        server,
                        federation::keys::get_keys::v1::Request {
                            device_keys: device_keys_input_fed,
                        },
                    ),
                )
                .await
                .map_err(|_e| Error::BadServerResponse("Query took too long"))
                .and_then(|result| result),
            )
        })
        .collect();

    while let Some((server, response)) = futures.next().await {
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                back_off(server.to_owned()).await;
                debug!(%server, %error, "remote device key query failed");
                failures.insert(server.to_string(), json!({}));
                continue;
            }
        };

        for (user, masterkey) in response.master_keys {
            let (master_key_id, mut master_key) =
                services().users.parse_master_key(&user, &masterkey)?;

            if let Some(our_master_key) = services().users.get_key(
                &master_key_id,
                sender_user,
                &user,
                &allowed_signatures,
            )? {
                let (_, our_master_key) = services()
                    .users
                    .parse_master_key(&user, &our_master_key)?;
                master_key.signatures.extend(our_master_key.signatures);
            }
            let json = serde_json::to_value(master_key)
                .expect("to_value always works");
            let raw = serde_json::from_value(json)
                .expect("Raw::from_value always works");
            services().users.add_cross_signing_keys(
                &user, &raw, &None, &None,
                // Dont notify. A notification would trigger another key
                // request resulting in an endless loop
                false,
            )?;
            master_keys.insert(user, raw);
        }

        self_signing_keys.extend(response.self_signing_keys);
        device_keys.extend(response.device_keys);
    }

    Ok(get_keys::v3::Response {
        failures,
        device_keys,
        master_keys,
        self_signing_keys,
        user_signing_keys,
    })
}

fn add_unsigned_device_display_name(
    keys: &mut Raw<ruma::encryption::DeviceKeys>,
    metadata: ruma::api::client::device::Device,
) -> serde_json::Result<()> {
    if let Some(display_name) = metadata.display_name {
        let mut object = keys
            .deserialize_as::<serde_json::Map<String, serde_json::Value>>()?;

        let unsigned = object.entry("unsigned").or_insert_with(|| json!({}));
        if let serde_json::Value::Object(unsigned_object) = unsigned {
            unsigned_object
                .insert("device_display_name".to_owned(), display_name.into());
        }

        *keys = Raw::from_json(serde_json::value::to_raw_value(&object)?);
    }

    Ok(())
}

pub(crate) async fn claim_keys_helper(
    one_time_keys_input: &BTreeMap<
        OwnedUserId,
        BTreeMap<OwnedDeviceId, DeviceKeyAlgorithm>,
    >,
) -> Result<claim_keys::v3::Response> {
    let mut one_time_keys = BTreeMap::new();

    let mut get_over_federation = BTreeMap::new();

    for (user_id, map) in one_time_keys_input {
        if user_id.server_name() != services().globals.server_name() {
            get_over_federation
                .entry(user_id.server_name())
                .or_insert_with(Vec::new)
                .push((user_id, map));
        }

        let mut container = BTreeMap::new();
        for (device_id, key_algorithm) in map {
            if let Some(one_time_keys) = services().users.take_one_time_key(
                user_id,
                device_id,
                key_algorithm,
            )? {
                let mut c = BTreeMap::new();
                c.insert(one_time_keys.0, one_time_keys.1);
                container.insert(device_id.clone(), c);
            }
        }
        one_time_keys.insert(user_id.clone(), container);
    }

    let mut failures = BTreeMap::new();

    let mut futures: FuturesUnordered<_> = get_over_federation
        .into_iter()
        .map(|(server, vec)| async move {
            let mut one_time_keys_input_fed = BTreeMap::new();
            for (user_id, keys) in vec {
                one_time_keys_input_fed.insert(user_id.clone(), keys.clone());
            }
            (
                server,
                services()
                    .sending
                    .send_federation_request(
                        server,
                        federation::keys::claim_keys::v1::Request {
                            one_time_keys: one_time_keys_input_fed,
                        },
                    )
                    .await,
            )
        })
        .collect();

    while let Some((server, response)) = futures.next().await {
        match response {
            Ok(keys) => {
                one_time_keys.extend(keys.one_time_keys);
            }
            Err(_e) => {
                failures.insert(server.to_string(), json!({}));
            }
        }
    }

    Ok(claim_keys::v3::Response {
        failures,
        one_time_keys,
    })
}

#[allow(clippy::type_complexity)]
fn deserialize_signing_key(
    key: &Option<Raw<CrossSigningKey>>,
) -> Result<
    Option<(
        BTreeMap<OwnedDeviceKeyId, String>,
        BTreeMap<OwnedUserId, BTreeMap<OwnedDeviceKeyId, String>>,
    )>,
> {
    let result =
        key.as_ref().map(Raw::deserialize).transpose().map_err(|_| {
            Error::BadRequest(ErrorKind::InvalidParam, "Invalid key JSON")
        })?;

    Ok(result.map(|key| (key.keys, key.signatures)))
}

fn upload_should_bypass_uia(
    sender_user: &UserId,
    body: &upload_signing_keys::v3::Request,
) -> Result<bool> {
    let master_key = services().users.get_master_key(
        Some(sender_user),
        sender_user,
        &|other| sender_user == other,
    )?;
    let self_signing_key = services().users.get_self_signing_key(
        Some(sender_user),
        sender_user,
        &|other| sender_user == other,
    )?;
    let user_signing_key =
        services().users.get_user_signing_key(sender_user)?;

    let master_eq = deserialize_signing_key(&master_key)?
        == deserialize_signing_key(&body.master_key)?;
    let self_signing_eq = deserialize_signing_key(&self_signing_key)?
        == deserialize_signing_key(&body.self_signing_key)?;
    let user_signing_eq = deserialize_signing_key(&user_signing_key)?
        == deserialize_signing_key(&body.user_signing_key)?;

    Ok(master_key.is_none()
        || (master_eq && self_signing_eq && user_signing_eq))
}
