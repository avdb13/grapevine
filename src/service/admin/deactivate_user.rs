use std::sync::Arc;

use clap::Parser;
use ruma::UserId;

use crate::{api::client_server::leave_all_rooms, services};

#[derive(Parser)]
#[command(version = env!("CARGO_PKG_VERSION"))]
struct Args {
    #[arg(short, long)]
    leave_rooms: bool,
    user_id: Box<UserId>,
}

pub(crate) enum Errors {
    Error(crate::utils::error::Error),
    NotFound,
    NotFrom
}

pub(crate) async fn deactivate_user(user_id: &UserId, leave_rooms: bool) -> Result<(), Errors> {
    let user_id = Arc::<UserId>::from(user_id);
    match services().users.exists(&user_id) {
        Ok(true) => {},
        Ok(false) => { return Err(Errors::NotFound); },
        Err(e) => { return Err(Errors::Error(e)); }
    }
    if user_id.server_name() != services().globals.server_name() {
        return Err(Errors::NotFrom);
    }
    if let Err(e) = services().users.deactivate_account(&user_id) {
        return Err(Errors::Error(e));
    }
    if leave_rooms {
        if let Err(e) = leave_all_rooms(&user_id).await {
            return Err(Errors::Error(e));
        }
    }
    Ok(())
}

pub(crate) async fn try_process(argv: Vec<&str>) -> Result<String, String> {
    let Ok(input) = Args::try_parse_from(argv) else {
        return Err("Incorrect Arguments".to_owned());
    };
    match deactivate_user(&input.user_id, input.leave_rooms).await {
        Ok(()) => { Ok(format!("User {} has been deactivated", input.user_id)) },
        Err(Errors::NotFrom) => Err(format!("User {} is not from this homeserver", input.user_id)),
        Err(Errors::NotFound) => Err(format!("User {} doesn't exist on this server", input.user_id)),
        Err(Errors::Error(e)) => Err(format!("There was an error while trying to deactivate user {}: {e:?}", input.user_id))
    }
}
