use ruma::api::client::{
    backup::{
        add_backup_keys, add_backup_keys_for_room, add_backup_keys_for_session,
        create_backup_version, delete_backup_keys, delete_backup_keys_for_room,
        delete_backup_keys_for_session, delete_backup_version, get_backup_info,
        get_backup_keys, get_backup_keys_for_room, get_backup_keys_for_session,
        get_latest_backup_info, update_backup_version,
    },
    error::ErrorKind,
};

use crate::{services, Ar, Error, Ra, Result};

/// # `POST /_matrix/client/r0/room_keys/version`
///
/// Creates a new backup.
pub(crate) async fn create_backup_version_route(
    body: Ar<create_backup_version::v3::Request>,
) -> Result<Ra<create_backup_version::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");
    let version =
        services().key_backups.create_backup(sender_user, &body.algorithm)?;

    Ok(Ra(create_backup_version::v3::Response {
        version,
    }))
}

/// # `PUT /_matrix/client/r0/room_keys/version/{version}`
///
/// Update information about an existing backup. Only `auth_data` can be
/// modified.
pub(crate) async fn update_backup_version_route(
    body: Ar<update_backup_version::v3::Request>,
) -> Result<Ra<update_backup_version::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");
    services().key_backups.update_backup(
        sender_user,
        &body.version,
        &body.algorithm,
    )?;

    Ok(Ra(update_backup_version::v3::Response {}))
}

/// # `GET /_matrix/client/r0/room_keys/version`
///
/// Get information about the latest backup version.
pub(crate) async fn get_latest_backup_info_route(
    body: Ar<get_latest_backup_info::v3::Request>,
) -> Result<Ra<get_latest_backup_info::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let (version, algorithm) = services()
        .key_backups
        .get_latest_backup(sender_user)?
        .ok_or(Error::BadRequest(
            ErrorKind::NotFound,
            "Key backup does not exist.",
        ))?;

    Ok(Ra(get_latest_backup_info::v3::Response {
        algorithm,
        count: services()
            .key_backups
            .count_keys(sender_user, &version)?
            .try_into()
            .expect("count should fit in UInt"),
        etag: services().key_backups.get_etag(sender_user, &version)?,
        version,
    }))
}

/// # `GET /_matrix/client/r0/room_keys/version`
///
/// Get information about an existing backup.
pub(crate) async fn get_backup_info_route(
    body: Ar<get_backup_info::v3::Request>,
) -> Result<Ra<get_backup_info::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");
    let algorithm = services()
        .key_backups
        .get_backup(sender_user, &body.version)?
        .ok_or(Error::BadRequest(
            ErrorKind::NotFound,
            "Key backup does not exist.",
        ))?;

    Ok(Ra(get_backup_info::v3::Response {
        algorithm,
        count: services()
            .key_backups
            .count_keys(sender_user, &body.version)?
            .try_into()
            .expect("count should fit in UInt"),
        etag: services().key_backups.get_etag(sender_user, &body.version)?,
        version: body.version.clone(),
    }))
}

/// # `DELETE /_matrix/client/r0/room_keys/version/{version}`
///
/// Delete an existing key backup.
///
/// - Deletes both information about the backup, as well as all key data related
///   to the backup
pub(crate) async fn delete_backup_version_route(
    body: Ar<delete_backup_version::v3::Request>,
) -> Result<Ra<delete_backup_version::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    services().key_backups.delete_backup(sender_user, &body.version)?;

    Ok(Ra(delete_backup_version::v3::Response {}))
}

/// # `PUT /_matrix/client/r0/room_keys/keys`
///
/// Add the received backup keys to the database.
///
/// - Only manipulating the most recently created version of the backup is
///   allowed
/// - Adds the keys to the backup
/// - Returns the new number of keys in this backup and the etag
pub(crate) async fn add_backup_keys_route(
    body: Ar<add_backup_keys::v3::Request>,
) -> Result<Ra<add_backup_keys::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    if Some(&body.version)
        != services()
            .key_backups
            .get_latest_backup_version(sender_user)?
            .as_ref()
    {
        return Err(Error::BadRequest(
            ErrorKind::InvalidParam,
            "You may only manipulate the most recently created version of the \
             backup.",
        ));
    }

    for (room_id, room) in &body.rooms {
        for (session_id, key_data) in &room.sessions {
            services().key_backups.add_key(
                sender_user,
                &body.version,
                room_id,
                session_id,
                key_data,
            )?;
        }
    }

    Ok(Ra(add_backup_keys::v3::Response {
        count: services()
            .key_backups
            .count_keys(sender_user, &body.version)?
            .try_into()
            .expect("count should fit in UInt"),
        etag: services().key_backups.get_etag(sender_user, &body.version)?,
    }))
}

/// # `PUT /_matrix/client/r0/room_keys/keys/{roomId}`
///
/// Add the received backup keys to the database.
///
/// - Only manipulating the most recently created version of the backup is
///   allowed
/// - Adds the keys to the backup
/// - Returns the new number of keys in this backup and the etag
pub(crate) async fn add_backup_keys_for_room_route(
    body: Ar<add_backup_keys_for_room::v3::Request>,
) -> Result<Ra<add_backup_keys_for_room::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    if Some(&body.version)
        != services()
            .key_backups
            .get_latest_backup_version(sender_user)?
            .as_ref()
    {
        return Err(Error::BadRequest(
            ErrorKind::InvalidParam,
            "You may only manipulate the most recently created version of the \
             backup.",
        ));
    }

    for (session_id, key_data) in &body.sessions {
        services().key_backups.add_key(
            sender_user,
            &body.version,
            &body.room_id,
            session_id,
            key_data,
        )?;
    }

    Ok(Ra(add_backup_keys_for_room::v3::Response {
        count: services()
            .key_backups
            .count_keys(sender_user, &body.version)?
            .try_into()
            .expect("count should fit in UInt"),
        etag: services().key_backups.get_etag(sender_user, &body.version)?,
    }))
}

/// # `PUT /_matrix/client/r0/room_keys/keys/{roomId}/{sessionId}`
///
/// Add the received backup key to the database.
///
/// - Only manipulating the most recently created version of the backup is
///   allowed
/// - Adds the keys to the backup
/// - Returns the new number of keys in this backup and the etag
pub(crate) async fn add_backup_keys_for_session_route(
    body: Ar<add_backup_keys_for_session::v3::Request>,
) -> Result<Ra<add_backup_keys_for_session::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    if Some(&body.version)
        != services()
            .key_backups
            .get_latest_backup_version(sender_user)?
            .as_ref()
    {
        return Err(Error::BadRequest(
            ErrorKind::InvalidParam,
            "You may only manipulate the most recently created version of the \
             backup.",
        ));
    }

    services().key_backups.add_key(
        sender_user,
        &body.version,
        &body.room_id,
        &body.session_id,
        &body.session_data,
    )?;

    Ok(Ra(add_backup_keys_for_session::v3::Response {
        count: services()
            .key_backups
            .count_keys(sender_user, &body.version)?
            .try_into()
            .expect("count should fit in UInt"),
        etag: services().key_backups.get_etag(sender_user, &body.version)?,
    }))
}

/// # `GET /_matrix/client/r0/room_keys/keys`
///
/// Retrieves all keys from the backup.
pub(crate) async fn get_backup_keys_route(
    body: Ar<get_backup_keys::v3::Request>,
) -> Result<Ra<get_backup_keys::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let rooms = services().key_backups.get_all(sender_user, &body.version)?;

    Ok(Ra(get_backup_keys::v3::Response {
        rooms,
    }))
}

/// # `GET /_matrix/client/r0/room_keys/keys/{roomId}`
///
/// Retrieves all keys from the backup for a given room.
pub(crate) async fn get_backup_keys_for_room_route(
    body: Ar<get_backup_keys_for_room::v3::Request>,
) -> Result<Ra<get_backup_keys_for_room::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let sessions = services().key_backups.get_room(
        sender_user,
        &body.version,
        &body.room_id,
    )?;

    Ok(Ra(get_backup_keys_for_room::v3::Response {
        sessions,
    }))
}

/// # `GET /_matrix/client/r0/room_keys/keys/{roomId}/{sessionId}`
///
/// Retrieves a key from the backup.
pub(crate) async fn get_backup_keys_for_session_route(
    body: Ar<get_backup_keys_for_session::v3::Request>,
) -> Result<Ra<get_backup_keys_for_session::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let key_data = services()
        .key_backups
        .get_session(
            sender_user,
            &body.version,
            &body.room_id,
            &body.session_id,
        )?
        .ok_or(Error::BadRequest(
            ErrorKind::NotFound,
            "Backup key not found for this user's session.",
        ))?;

    Ok(Ra(get_backup_keys_for_session::v3::Response {
        key_data,
    }))
}

/// # `DELETE /_matrix/client/r0/room_keys/keys`
///
/// Delete the keys from the backup.
pub(crate) async fn delete_backup_keys_route(
    body: Ar<delete_backup_keys::v3::Request>,
) -> Result<Ra<delete_backup_keys::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    services().key_backups.delete_all_keys(sender_user, &body.version)?;

    Ok(Ra(delete_backup_keys::v3::Response {
        count: services()
            .key_backups
            .count_keys(sender_user, &body.version)?
            .try_into()
            .expect("count should fit in UInt"),
        etag: services().key_backups.get_etag(sender_user, &body.version)?,
    }))
}

/// # `DELETE /_matrix/client/r0/room_keys/keys/{roomId}`
///
/// Delete the keys from the backup for a given room.
pub(crate) async fn delete_backup_keys_for_room_route(
    body: Ar<delete_backup_keys_for_room::v3::Request>,
) -> Result<Ra<delete_backup_keys_for_room::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    services().key_backups.delete_room_keys(
        sender_user,
        &body.version,
        &body.room_id,
    )?;

    Ok(Ra(delete_backup_keys_for_room::v3::Response {
        count: services()
            .key_backups
            .count_keys(sender_user, &body.version)?
            .try_into()
            .expect("count should fit in UInt"),
        etag: services().key_backups.get_etag(sender_user, &body.version)?,
    }))
}

/// # `DELETE /_matrix/client/r0/room_keys/keys/{roomId}/{sessionId}`
///
/// Delete a key from the backup.
pub(crate) async fn delete_backup_keys_for_session_route(
    body: Ar<delete_backup_keys_for_session::v3::Request>,
) -> Result<Ra<delete_backup_keys_for_session::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    services().key_backups.delete_room_key(
        sender_user,
        &body.version,
        &body.room_id,
        &body.session_id,
    )?;

    Ok(Ra(delete_backup_keys_for_session::v3::Response {
        count: services()
            .key_backups
            .count_keys(sender_user, &body.version)?
            .try_into()
            .expect("count should fit in UInt"),
        etag: services().key_backups.get_etag(sender_user, &body.version)?,
    }))
}
