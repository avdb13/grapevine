use ruma::api::client::{
    error::ErrorKind,
    filter::{create_filter, get_filter},
};

use crate::{services, Ar, Error, Ra, Result};

/// # `GET /_matrix/client/r0/user/{userId}/filter/{filterId}`
///
/// Loads a filter that was previously created.
///
/// - A user can only access their own filters
pub(crate) async fn get_filter_route(
    body: Ar<get_filter::v3::Request>,
) -> Result<Ra<get_filter::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");
    let Some(filter) =
        services().users.get_filter(sender_user, &body.filter_id)?
    else {
        return Err(Error::BadRequest(
            ErrorKind::NotFound,
            "Filter not found.",
        ));
    };

    Ok(Ra(get_filter::v3::Response::new(filter)))
}

/// # `PUT /_matrix/client/r0/user/{userId}/filter`
///
/// Creates a new filter to be used by other endpoints.
pub(crate) async fn create_filter_route(
    body: Ar<create_filter::v3::Request>,
) -> Result<Ra<create_filter::v3::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");
    Ok(Ra(create_filter::v3::Response::new(
        services().users.create_filter(sender_user, &body.filter)?,
    )))
}
