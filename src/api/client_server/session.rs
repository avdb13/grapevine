use ruma::{
    api::client::{
        error::ErrorKind,
        session::{
            get_login_types::{
                self,
                v3::{ApplicationServiceLoginType, PasswordLoginType},
            },
            login, logout, logout_all,
        },
        uiaa::UserIdentifier,
    },
    UserId,
};
use serde::Deserialize;
use tracing::{info, warn};

use super::{DEVICE_ID_LENGTH, TOKEN_LENGTH};
use crate::{services, utils, Ar, Error, Ra, Result};

#[derive(Debug, Deserialize)]
struct Claims {
    sub: String,
}

/// # `GET /_matrix/client/r0/login`
///
/// Get the supported login types of this server. One of these should be used as
/// the `type` field when logging in.
pub(crate) async fn get_login_types_route(
    _body: Ar<get_login_types::v3::Request>,
) -> Result<Ra<get_login_types::v3::Response>> {
    Ok(Ra(get_login_types::v3::Response::new(vec![
        get_login_types::v3::LoginType::Password(PasswordLoginType::default()),
        get_login_types::v3::LoginType::ApplicationService(
            ApplicationServiceLoginType::default(),
        ),
    ])))
}

/// # `POST /_matrix/client/r0/login`
///
/// Authenticates the user and returns an access token it can use in subsequent
/// requests.
///
/// - The user needs to authenticate using their password (or if enabled using a
///   json web token)
/// - If `device_id` is known: invalidates old access token of that device
/// - If `device_id` is unknown: creates a new device
/// - Returns access token that is associated with the user and device
///
/// Note: You can use [`GET /_matrix/client/r0/login`](get_login_types_route) to
/// see supported login types.
#[allow(
    // To allow deprecated login methods
    deprecated,
    clippy::too_many_lines,
)]
pub(crate) async fn login_route(
    body: Ar<login::v3::Request>,
) -> Result<Ra<login::v3::Response>> {
    // Validate login method
    // TODO: Other login methods
    let user_id = match &body.login_info {
        login::v3::LoginInfo::Password(login::v3::Password {
            identifier,
            password,
            user,
            ..
        }) => {
            let user_id =
                if let Some(UserIdentifier::UserIdOrLocalpart(user_id)) =
                    identifier
                {
                    UserId::parse_with_server_name(
                        user_id.to_lowercase(),
                        services().globals.server_name(),
                    )
                } else if let Some(user) = user {
                    UserId::parse(user)
                } else {
                    warn!(kind = ?body.login_info, "Bad login kind");
                    return Err(Error::BadRequest(
                        ErrorKind::forbidden(),
                        "Bad login type.",
                    ));
                }
                .map_err(|_| {
                    Error::BadRequest(
                        ErrorKind::InvalidUsername,
                        "Username is invalid.",
                    )
                })?;

            if services().appservice.is_exclusive_user_id(&user_id).await {
                return Err(Error::BadRequest(
                    ErrorKind::Exclusive,
                    "User id reserved by appservice.",
                ));
            }

            let hash = services().users.password_hash(&user_id)?.ok_or(
                Error::BadRequest(
                    ErrorKind::forbidden(),
                    "Wrong username or password.",
                ),
            )?;

            if hash.is_empty() {
                return Err(Error::BadRequest(
                    ErrorKind::UserDeactivated,
                    "The user has been deactivated",
                ));
            }

            if !utils::verify_password(hash, password) {
                return Err(Error::BadRequest(
                    ErrorKind::forbidden(),
                    "Wrong username or password.",
                ));
            }

            user_id
        }
        login::v3::LoginInfo::Token(login::v3::Token {
            token,
        }) => {
            if let Some(jwt_decoding_key) =
                services().globals.jwt_decoding_key()
            {
                let token = jsonwebtoken::decode::<Claims>(
                    token,
                    jwt_decoding_key,
                    &jsonwebtoken::Validation::default(),
                )
                .map_err(|_| {
                    Error::BadRequest(
                        ErrorKind::InvalidUsername,
                        "Token is invalid.",
                    )
                })?;
                let username = token.claims.sub.to_lowercase();
                let user_id = UserId::parse_with_server_name(
                    username,
                    services().globals.server_name(),
                )
                .map_err(|_| {
                    Error::BadRequest(
                        ErrorKind::InvalidUsername,
                        "Username is invalid.",
                    )
                })?;

                if services().appservice.is_exclusive_user_id(&user_id).await {
                    return Err(Error::BadRequest(
                        ErrorKind::Exclusive,
                        "User id reserved by appservice.",
                    ));
                }

                user_id
            } else {
                return Err(Error::BadRequest(
                    ErrorKind::Unknown,
                    "Token login is not supported (server has no jwt decoding \
                     key).",
                ));
            }
        }
        login::v3::LoginInfo::ApplicationService(
            login::v3::ApplicationService {
                identifier,
                user,
            },
        ) => {
            let user_id =
                if let Some(UserIdentifier::UserIdOrLocalpart(user_id)) =
                    identifier
                {
                    UserId::parse_with_server_name(
                        user_id.to_lowercase(),
                        services().globals.server_name(),
                    )
                } else if let Some(user) = user {
                    UserId::parse(user)
                } else {
                    warn!(kind = ?body.login_info, "Bad login kind");
                    return Err(Error::BadRequest(
                        ErrorKind::forbidden(),
                        "Bad login type.",
                    ));
                }
                .map_err(|_| {
                    Error::BadRequest(
                        ErrorKind::InvalidUsername,
                        "Username is invalid.",
                    )
                })?;

            if let Some(info) = &body.appservice_info {
                if !info.is_user_match(&user_id) {
                    return Err(Error::BadRequest(
                        ErrorKind::Exclusive,
                        "User is not in namespace.",
                    ));
                }
            } else {
                return Err(Error::BadRequest(
                    ErrorKind::MissingToken,
                    "Missing appservice token.",
                ));
            }

            user_id
        }
        _ => {
            warn!(kind = ?body.login_info, "Unsupported or unknown login kind");
            return Err(Error::BadRequest(
                ErrorKind::Unknown,
                "Unsupported login type.",
            ));
        }
    };

    // Generate new device id if the user didn't specify one
    let device_id = body
        .device_id
        .clone()
        .unwrap_or_else(|| utils::random_string(DEVICE_ID_LENGTH).into());

    // Generate a new token for the device
    let token = utils::random_string(TOKEN_LENGTH);

    // Determine if device_id was provided and exists in the db for this user
    let device_exists = body.device_id.as_ref().map_or(false, |device_id| {
        services()
            .users
            .all_device_ids(&user_id)
            .any(|x| x.as_ref().map_or(false, |v| v == device_id))
    });

    if device_exists {
        services().users.set_token(&user_id, &device_id, &token)?;
    } else {
        services().users.create_device(
            &user_id,
            &device_id,
            &token,
            body.initial_device_display_name.clone(),
        )?;
    }

    info!(%user_id, %device_id, "User logged in");

    // Homeservers are still required to send the `home_server` field
    #[allow(deprecated)]
    Ok(Ra(login::v3::Response {
        user_id,
        access_token: token,
        home_server: Some(services().globals.server_name().to_owned()),
        device_id,
        well_known: None,
        refresh_token: None,
        expires_in: None,
    }))
}

/// # `POST /_matrix/client/r0/logout`
///
/// Log out the current device.
///
/// - Invalidates access token
/// - Deletes device metadata (device id, device display name, last seen ip,
///   last seen ts)
/// - Forgets to-device events
/// - Triggers device list updates
pub(crate) async fn logout_route(
    body: Ar<logout::v3::Request>,
) -> Result<Ra<logout::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");
    let sender_device =
        body.sender_device.as_ref().expect("user is authenticated");

    if let Some(info) = &body.appservice_info {
        if !info.is_user_match(sender_user) {
            return Err(Error::BadRequest(
                ErrorKind::Exclusive,
                "User is not in namespace.",
            ));
        }
    }

    services().users.remove_device(sender_user, sender_device)?;

    info!(user_id = %sender_user, device_id = %sender_device, "User logged out");

    Ok(Ra(logout::v3::Response::new()))
}

/// # `POST /_matrix/client/r0/logout/all`
///
/// Log out all devices of this user.
///
/// - Invalidates all access tokens
/// - Deletes all device metadata (device id, device display name, last seen ip,
///   last seen ts)
/// - Forgets all to-device events
/// - Triggers device list updates
///
/// Note: This is equivalent to calling [`GET
/// /_matrix/client/r0/logout`](logout_route) from each device of this user.
pub(crate) async fn logout_all_route(
    body: Ar<logout_all::v3::Request>,
) -> Result<Ra<logout_all::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    if let Some(info) = &body.appservice_info {
        if !info.is_user_match(sender_user) {
            return Err(Error::BadRequest(
                ErrorKind::Exclusive,
                "User is not in namespace.",
            ));
        }
    } else {
        return Err(Error::BadRequest(
            ErrorKind::MissingToken,
            "Missing appservice token.",
        ));
    }

    for device_id in services().users.all_device_ids(sender_user).flatten() {
        services().users.remove_device(sender_user, &device_id)?;
    }

    Ok(Ra(logout_all::v3::Response::new()))
}
