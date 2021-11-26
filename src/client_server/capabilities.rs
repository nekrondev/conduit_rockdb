use crate::{ConduitResult, Ruma};
use ruma::{
    api::client::r0::capabilities::{
        get_capabilities, Capabilities, RoomVersionStability, RoomVersionsCapability,
    },
    RoomVersionId,
};
use std::collections::BTreeMap;

#[cfg(feature = "conduit_bin")]
use rocket::get;

/// # `GET /_matrix/client/r0/capabilities`
///
/// Get information on the supported feature set and other relevent capabilities of this server.
#[cfg_attr(
    feature = "conduit_bin",
    get("/_matrix/client/r0/capabilities", data = "<_body>")
)]
#[tracing::instrument(skip(_body))]
pub async fn get_capabilities_route(
    _body: Ruma<get_capabilities::Request>,
) -> ConduitResult<get_capabilities::Response> {
    let mut available = BTreeMap::new();
    available.insert(RoomVersionId::V5, RoomVersionStability::Stable);
    available.insert(RoomVersionId::V6, RoomVersionStability::Stable);

    let mut capabilities = Capabilities::new();
    capabilities.room_versions = RoomVersionsCapability {
        default: RoomVersionId::V6,
        available,
    };

    Ok(get_capabilities::Response { capabilities }.into())
}
