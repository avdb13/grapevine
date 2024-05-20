use std::collections::BTreeMap;

use ruma::api::client::thirdparty::get_protocols;

use crate::{Ra, Result, Ruma};

/// # `GET /_matrix/client/r0/thirdparty/protocols`
///
/// TODO: Fetches all metadata about protocols supported by the homeserver.
pub(crate) async fn get_protocols_route(
    _body: Ruma<get_protocols::v3::Request>,
) -> Result<Ra<get_protocols::v3::Response>> {
    // TODO
    Ok(Ra(get_protocols::v3::Response {
        protocols: BTreeMap::new(),
    }))
}
