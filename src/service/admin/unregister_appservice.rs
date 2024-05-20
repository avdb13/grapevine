use clap::Parser;

use crate::services;

#[derive(Parser)]
#[command(version = env!("CARGO_PKG_VERSION"))]
struct Args {
    appservice_identifier: String,
}

pub(crate) async fn try_process(argv: Vec<&str>) -> Result<String, String> {
    let Ok(input) = Args::try_parse_from(argv) else {
        return Err("Incorrect Arguments".to_owned());
    };
    match services()
        .appservice
        .unregister_appservice(&input.appservice_identifier)
        .await
    {
        Ok(()) => Ok("Appservice unregistered.".to_owned()),
        Err(e) => Err(format!("Failed to unregister appservice: {e}")),
    }
}
