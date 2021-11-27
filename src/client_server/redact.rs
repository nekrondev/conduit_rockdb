use std::sync::Arc;

use crate::{database::DatabaseGuard, pdu::PduBuilder, ConduitResult, Ruma};
use ruma::{
    api::client::r0::redact::redact_event,
    events::{room::redaction::RoomRedactionEventContent, EventType},
};

#[cfg(feature = "conduit_bin")]
use rocket::put;
use serde_json::value::to_raw_value;

/// # `PUT /_matrix/client/r0/rooms/{roomId}/redact/{eventId}/{txnId}`
///
/// Tries to send a redaction event into the room.
///
/// - TODO: Handle txn id
#[cfg_attr(
    feature = "conduit_bin",
    put("/_matrix/client/r0/rooms/<_>/redact/<_>/<_>", data = "<req>")
)]
#[tracing::instrument(skip(db, req))]
pub async fn redact_event_route(
    db: DatabaseGuard,
    req: Ruma<redact_event::Request<'_>>,
) -> ConduitResult<redact_event::Response> {
    let body = req.body;
    let sender_user = req.sender_user.as_ref().expect("user is authenticated");

    let mutex_state = Arc::clone(
        db.globals
            .roomid_mutex_state
            .write()
            .unwrap()
            .entry(body.room_id.clone())
            .or_default(),
    );
    let state_lock = mutex_state.lock().await;

    let event_id = db.rooms.build_and_append_pdu(
        PduBuilder {
            event_type: EventType::RoomRedaction,
            content: to_raw_value(&RoomRedactionEventContent {
                reason: body.reason.clone(),
            })
            .expect("event is valid, we just created it"),
            unsigned: None,
            state_key: None,
            redacts: Some(body.event_id.into()),
        },
        sender_user,
        &body.room_id,
        &db,
        &state_lock,
    )?;

    drop(state_lock);

    db.flush()?;

    let event_id = (*event_id).to_owned();
    Ok(redact_event::Response { event_id }.into())
}
