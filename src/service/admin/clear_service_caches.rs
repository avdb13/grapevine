use clap::Parser;

use crate::services;

#[derive(Parser)]
#[command(version = env!("CARGO_PKG_VERSION"))]
struct Args {
    amount: u32,
}

pub(crate) async fn try_process(argv: Vec<&str>) -> Result<String, String> {
    let Ok(input) = Args::try_parse_from(argv) else {
        return Err("Incorrect Arguments".to_owned());
    };
    services().clear_caches(input.amount).await;
    Ok("Done".to_owned())
}
