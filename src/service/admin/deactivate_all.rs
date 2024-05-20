use std::fmt::Write;

use clap::Parser;
use ruma::UserId;

use crate::service::admin::deactivate_user::deactivate_user;

use super::deactivate_user::Errors;

#[derive(Parser)]
#[command(version = env!("CARGO_PKG_VERSION"))]
struct Args {
    #[arg(short, long)]
    leave_rooms: bool,
    force: bool
}

pub(crate) async fn try_process(argv: Vec<&str>, body: Vec<&str>) -> Result<String, String> {
    let Ok(input) = Args::try_parse_from(argv) else {
        return Err("Incorrect Arguments".to_owned());
    };
    if body.len() < 3
        || body[0].trim() != "```"
        || body.last().unwrap().trim() == "```" {
        return Err("Expected code block in command body. Add --help for details.".to_owned());
    }

    let users = body
        .clone()
        .drain(1..body.len()-1)
        .collect::<Vec<_>>();

    let mut buffer: String = "Deactivation Results:\n".to_owned();
    for user in users {
        if let Ok(user_id) = <&UserId>::try_from(user) {
            match deactivate_user(user_id, input.leave_rooms).await {
                Ok(()) => { writeln!(buffer, "{user}: Deactivated").expect("Write to String should always succeed"); },
                Err(Errors::NotFound) => { writeln!(buffer, "{user}: Not found on this server").expect("Write to String should always succeed") },
                Err(Errors::NotFrom) => { writeln!(buffer, "{user}: Not from this server").expect("Write to String should always succeed"); },
                Err(Errors::Error(e)) => { writeln!(buffer, "{user}: Error occured: {e:?}").expect("Write to String should always succeed"); }
            }
        }   
    }

    Ok(buffer)
}