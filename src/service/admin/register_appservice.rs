use ruma::api::appservice::Registration;

use crate::services;

pub(crate) async fn try_process(body: Vec<&str>) -> Result<String, String> {
    if body.len() < 3
        || body[0].trim() != "```"
        || body.last().unwrap().trim() == "```"
    {
        return Err(
            "Expected code block in command body. Add --help for details."
                .to_owned(),
        );
    }
    let appservice_config = body[1..body.len() - 1].join("\n");
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
