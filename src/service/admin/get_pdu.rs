use clap::Parser;
use ruma::{CanonicalJsonObject, EventId};

use crate::services;

#[derive(Parser)]
#[command(version = env!("CARGO_PKG_VERSION"))]
struct Args {
    event_id: Box<EventId>,
}

pub(crate) fn get_pdu_json(
    event_id: &EventId,
) -> Result<Option<CanonicalJsonObject>, String> {
    match services().rooms.timeline.get_pdu_json(event_id) {
        Ok(json) => Ok(json),
        Err(e) => Err(format!("{e:?}")),
    }
}

pub(crate) fn try_process(argv: Vec<&str>) -> Result<String, String> {
    let Ok(input) = Args::try_parse_from(argv) else {
        return Err("Incorrect Arguments".to_owned());
    };
    let Ok(pdu_json) = get_pdu_json(&input.event_id) else {
        return Err("Failed to get PDU JSON".to_owned());
    };
    let Some(json) = pdu_json else {
        return Err("PDU not found".to_owned());
    };
    let json_text = serde_json::to_string_pretty(&json)
        .expect("Canonical JSON is valid JSON");
    Ok(format!("```json\n{json_text}\n```"))
}
