use std::{sync::Arc, time::Instant};

use clap::Parser;
use ruma::{EventId, RoomId};

use super::get_pdu::get_pdu_json;
use crate::services;

#[derive(Parser)]
#[command(version = env!("CARGO_PKG_VERSION"))]
struct Args {
    event_id: Box<EventId>,
}

pub(crate) async fn try_process(argv: Vec<&str>) -> Result<String, String> {
    let Ok(input) = Args::try_parse_from(argv) else {
        return Err("Incorrect Arguments".to_owned());
    };
    let event_id = Arc::<EventId>::from(input.event_id);

    let Ok(pdu_json) = get_pdu_json(&event_id) else {
        return Err("Failed to get PDU JSON".to_owned());
    };
    let Some(event) = pdu_json else {
        return Err("Event not found".to_owned());
    };
    let Some(room_id_str) = event.get("room_id").and_then(|val| val.as_str())
    else {
        return Err("Invalid event in database".to_owned());
    };
    let Ok(room_id) = <&RoomId>::try_from(room_id_str) else {
        return Err("Invalid room id field in event in database".to_owned());
    };
    let start = Instant::now();
    let Ok(chain) = services()
        .rooms
        .auth_chain
        .get_auth_chain(room_id, vec![event_id])
        .await
    else {
        return Err("Failed to retrieve auth chain from database".to_owned());
    };
    let elapsed = start.elapsed();
    Ok(format!(
        "Loaded auth chain with length {} in {elapsed:?}",
        chain.count()
    ))
}
