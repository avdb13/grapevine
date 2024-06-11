use crate::{service::admin::common::extract_code_block, services};

pub(crate) fn try_process(body: Vec<&str>) -> Result<String, String> {
    let string = extract_code_block(&body)?.join("\n");
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
