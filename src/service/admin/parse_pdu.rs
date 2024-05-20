use ruma::{EventId, RoomVersionId};

use crate::PduEvent;

pub(crate) fn try_process(body: &Vec<&str>) -> Result<String, String> {
    if body.len() > 2
        && body[0].trim() == "```"
        && body.last().unwrap().trim() == "```"
    {
        let string = body[1..body.len() - 1].join("\n");
        match serde_json::from_str(&string) {
            Ok(value) => {
                match ruma::signatures::reference_hash(
                    &value,
                    &RoomVersionId::V6,
                ) {
                    Ok(hash) => {
                        let event_id = EventId::parse(format!("${hash}"));

                        match serde_json::from_value::<PduEvent>(
                            serde_json::to_value(value).expect("value is json"),
                        ) {
                            Ok(pdu) => Ok(format!(
                                "EventId: {event_id:?}\\
                                                     n{pdu:#?}"
                            )),
                            Err(e) => Err(format!(
                                "EventId: {event_id:?}\\
                                                     nCould not parse event: \
                                 {e}"
                            )),
                        }
                    }
                    Err(e) => Err(format!("Could not parse PDU JSON: {e:?}")),
                }
            }
            Err(e) => Err(format!("Invalid json in command body: {e}")),
        }
    } else {
        Err("Expected code block in command body.".to_owned())
    }
}
