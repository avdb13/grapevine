use crate::services;

pub(crate) async fn try_process() -> Result<String, String> {
    let response1 = services().memory_usage().await;
    let response2 = services().globals.db.memory_usage();

    Ok(format!("Services:\n{response1}\n\nDatabase:\n{response2}"))
}
