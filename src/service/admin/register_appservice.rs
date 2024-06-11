use ruma::api::appservice::Registration;

use crate::{service::admin::common::extract_code_block, services};

pub(crate) async fn try_process(body: Vec<&str>) -> Result<String, String> {
    let appservice_config = extract_code_block(&body)?.join("\n");
    let parsed_config =
        serde_yaml::from_str::<Registration>(&appservice_config);
    match parsed_config {
        Ok(yaml) => {
            match services().appservice.register_appservice(yaml).await {
                Ok(id) => Ok(format!("Appservice registered with ID: {id}.")),
                Err(e) => Err(format!("Failed to register appservice: {e}")),
            }
        }
        Err(e) => Err(format!("Could not parse appservice config: {e}")),
    }
}
