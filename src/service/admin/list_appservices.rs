use crate::services;

pub(crate) async fn try_process() -> Result<String, String> {
    let appservices = services().appservice.iter_ids().await;
    let output = format!(
        "Appservices ({}): {}",
        appservices.len(),
        appservices.join(", ")
    );
    Ok(output)
}
