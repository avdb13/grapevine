use ruma::{api::client::space::get_hierarchy, uint};

use crate::{services, Ar, Ra, Result};

/// # `GET /_matrix/client/v1/rooms/{room_id}/hierarchy`
///
/// Paginates over the space tree in a depth-first manner to locate child rooms
/// of a given space.
pub(crate) async fn get_hierarchy_route(
    body: Ar<get_hierarchy::v1::Request>,
) -> Result<Ra<get_hierarchy::v1::Response>> {
    let sender_user = body.sender_user.as_ref().expect("user is authenticated");

    let skip =
        body.from.as_ref().and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);

    let limit = body
        .limit
        .map(|x| x.min(uint!(100)))
        .unwrap_or(uint!(10))
        .try_into()
        .expect("0-100 should fit in usize");

    // Plus one to skip the space room itself
    let max_depth = usize::try_from(
        body.max_depth.map(|x| x.min(uint!(10))).unwrap_or(uint!(3)),
    )
    .expect("0-10 should fit in usize")
        + 1;

    services()
        .rooms
        .spaces
        .get_hierarchy(
            sender_user,
            &body.room_id,
            limit,
            skip,
            max_depth,
            body.suggested_only,
        )
        .await
        .map(Ra)
}
