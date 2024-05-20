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
    if let Err(e) = services().rooms.metadata.disable_room(&input.room_id, true) {
        return Err(format!("Error disabling room: {e:?}"));
    }
    Ok("Room Disabled".to_owned())
}