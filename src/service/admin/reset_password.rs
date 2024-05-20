use clap::Parser;
use ruma::UserId;

use crate::{api::client_server::AUTO_GEN_PASSWORD_LENGTH, services, utils};

#[derive(Parser)]
#[command(version = env!("CARGO_PKG_VERSION"))]
struct Args {
    username: String,
}

pub(crate) fn try_process(argv: Vec<&str>) -> Result<String, String> {
    let Ok(input) = Args::try_parse_from(argv) else {
        return Err("Incorrect Arguments".to_owned());
    };
    let user_id = match UserId::parse_with_server_name(
        input.username.as_str().to_lowercase(),
        services().globals.server_name(),
    ) {
        Ok(id) => id,
        Err(e) => {
            return Err(format!(
                "The supplied username is not a valid username: {e}"
            ))
        }
    };

    // Checks if user is local
    if user_id.server_name() != services().globals.server_name() {
        return Err("The specified user is not from this server!".to_owned());
    };

    // Check if the specified user exists
    match services().users.exists(&user_id) {
        Ok(true) => {
            if user_id.localpart()
                == if services().globals.config.conduit_compat {
                    "conduit"
                } else {
                    "grapevine"
                }
            {
                return Err("Can't change password of admin bot".to_owned());
            }
        }
        Ok(false) => return Err("The specified user does not exist".to_owned()),
        Err(e) => {
            return Err(format!(
                "An error occured while checking if the account already \
                 exists: {e:?}"
            ))
        }
    }

    let new_password = utils::random_string(AUTO_GEN_PASSWORD_LENGTH);

    match services().users.set_password(&user_id, Some(new_password.as_str())) {
        Ok(()) => Ok(format!(
            "Successfully reset the password for user {user_id}: \
             {new_password}"
        )),
        Err(e) => {
            Err(format!("Couldn't reset the password for user {user_id}: {e}"))
        }
    }
}
