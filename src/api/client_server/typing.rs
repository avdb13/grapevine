use ruma::api::client::{error::ErrorKind, typing::create_typing_event};

use crate::{services, utils, Ar, Error, Ra, Result};

/// # `PUT /_matrix/client/r0/rooms/{roomId}/typing/{userId}`
///
/// Sets the typing state of the sender user.
pub(crate) async fn create_typing_event_route(
    body: Ar<create_typing_event::v3::Request>,
) -> Result<Ra<create_typing_event::v3::Response>> {
    use create_typing_event::v3::Typing;

    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    if !services().rooms.state_cache.is_joined(sender_user, &body.room_id)? {
        return Err(Error::BadRequest(
            ErrorKind::forbidden(),
            "You are not in this room.",
        ));
    }

    if let Typing::Yes(duration) = body.state {
        services()
            .rooms
            .edus
            .typing
            .typing_add(
                sender_user,
                &body.room_id,
                duration.as_millis().try_into().unwrap_or(u64::MAX)
                    + utils::millis_since_unix_epoch(),
            )
            .await?;
    } else {
        services()
            .rooms
            .edus
            .typing
            .typing_remove(sender_user, &body.room_id)
            .await?;
    }

    Ok(Ra(create_typing_event::v3::Response {}))
}
