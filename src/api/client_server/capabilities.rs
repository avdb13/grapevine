use std::collections::BTreeMap;

use ruma::api::client::discovery::get_capabilities::{
    self, Capabilities, RoomVersionStability, RoomVersionsCapability,
};

use crate::{services, Ra, Result, Ruma};

/// # `GET /_matrix/client/r0/capabilities`
///
/// Get information on the supported feature set and other relevent capabilities
/// of this server.
pub(crate) async fn get_capabilities_route(
    _body: Ruma<get_capabilities::v3::Request>,
) -> Result<Ra<get_capabilities::v3::Response>> {
    let mut available = BTreeMap::new();
    for room_version in &services().globals.unstable_room_versions {
        available.insert(room_version.clone(), RoomVersionStability::Unstable);
    }
    for room_version in &services().globals.stable_room_versions {
        available.insert(room_version.clone(), RoomVersionStability::Stable);
    }

    let mut capabilities = Capabilities::new();
    capabilities.room_versions = RoomVersionsCapability {
        default: services().globals.default_room_version(),
        available,
    };

    Ok(Ra(get_capabilities::v3::Response {
        capabilities,
    }))
}
