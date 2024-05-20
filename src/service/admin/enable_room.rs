use clap::Parser;
use ruma::RoomId;

use crate::services;

#[derive(Parser)]
#[command(version = env!("CARGO_PKG_VERSION"))]
struct Args {
    room_id: Box<RoomId>
}

pub(crate) fn try_process(argv: Vec<&str>) -> Result<String, String> {
    let Ok(input) = Args::try_parse_from(argv) else {
        return Err("Incorrect Arguments".to_owned());
    };
    if let Err(e) = services().rooms.metadata.disable_room(&input.room_id, false) {
        return Err(format!("Error enabling room: {e:?}"));
    }
    Ok("Room Enabled".to_owned())
}