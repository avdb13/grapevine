use crate::services;

pub(crate) fn try_process(body: &Vec<&str>) -> Result<String, String> {
    if body.len() < 3
        || body[0].trim() != "```"
        || body.last().unwrap().trim() == "```"
    {
        return Err("Expected code block in command body. Add --help for \
                    details."
            .to_owned());
    }
    let string = body[1..body.len() - 1].join("\n");
    match serde_json::from_str(&string) {
        Ok(mut value) => {
            ruma::signatures::sign_json(
                services().globals.server_name().as_str(),
                services().globals.keypair(),
                &mut value,
            )
            .expect("our request json is what ruma expects");
            let json_text = serde_json::to_string_pretty(&value)
                .expect("canonical json is valid json");
            Ok(json_text)
        }
        Err(e) => Err(format!("Invalid json: {e}")),
    }
}
