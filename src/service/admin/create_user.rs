use clap::Parser;
use ruma::{
    events::push_rules::{PushRulesEvent, PushRulesEventContent},
    UserId,
};

use crate::{api::client_server::AUTO_GEN_PASSWORD_LENGTH, services, utils};

#[derive(Parser)]
#[command(version = env!("CARGO_PKG_VERSION"))]
struct Args {
    username: String,
    password: Option<String>,
}

pub(crate) fn try_process(argv: Vec<&str>) -> Result<String, String> {
    let Ok(input) = Args::try_parse_from(argv) else {
        return Err("Incorrect Arguments".to_owned());
    };
    let password = input
        .password
        .unwrap_or_else(|| utils::random_string(AUTO_GEN_PASSWORD_LENGTH));
    // Validate user id
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
    // Test if the proposed user id is allowed historically
    if user_id.is_historical() {
        return Err(format!(
            "Userid {user_id} is not allowed due to historical"
        ));
    }
    // Test if the user already exists
    match services().users.exists(&user_id) {
        Ok(false) => {},
        Ok(true) => { return Err(format!("Userid {user_id} already exists")); },
        Err(e) => { return Err(format!("An error occured while checking if the account already exists: {e:?}")) }
    }

    if let Err(e) = services().users.create(&user_id, Some(password.as_str())) {
        return Err(format!("Failed to create user {e:?}"));
    }

    // Default to pretty displayname
    let displayname = user_id.localpart().to_owned();

    if let Err(e) =
        services().users.set_displayname(&user_id, Some(displayname))
    {
        return Err(format!("Failed to set display name of new user: {e:?}"));
    }

    // Initial account data
    if let Err(e) = services().account_data.update(
        None,
        &user_id,
        ruma::events::GlobalAccountDataEventType::PushRules.to_string().into(),
        &serde_json::to_value(PushRulesEvent {
            content: PushRulesEventContent {
                global: ruma::push::Ruleset::server_default(&user_id),
            },
        })
        .expect("to json value always works"),
    ) {
        return Err(format!("Failed to set initial account data: {e:?}"));
    }

    // we dont add a device since we're not the user, just the
    // creator

    // Inhibit login does not work for guests
    Ok(format!(
        "Created user with user_id: {user_id} and password: \
         {password}"
    ))
}
