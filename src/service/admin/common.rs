use ruma::{OwnedUserId, UserId};
use crate::services;

pub(crate) fn validate_username<S: Into<String>>(username: S) -> Result<OwnedUserId, String> {
    match UserId::parse_with_server_name(
        username.into().to_lowercase(),
        services().globals.server_name(),
    ) {
        Ok(id) => Ok(id),
        Err(e) => {
            return Err(format!(
                "The supplied username is not a valid username: {e}"
            ))
        }
    }
}

pub(crate) fn extract_code_block(body: Vec<&str>) -> Result<Vec<&str>, String> {
    if body.len() < 3
        || body[0].trim() != "```"
        || body.last().unwrap().trim() != "```" {
        return Err("Expected code block in command body. Add --help for more details.".to_owned());
    }
    let code_block = body[1..body.len() - 1].to_vec();
    Ok(code_block)
}