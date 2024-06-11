use ruma::{OwnedUserId, UserId};

use crate::services;

pub(crate) fn validate_username<S: Into<String>>(
    username: S,
) -> Result<OwnedUserId, String> {
    match UserId::parse_with_server_name(
        username.into().to_lowercase(),
        services().globals.server_name(),
    ) {
        Ok(id) => Ok(id),
        Err(e) => {
            Err(format!("The supplied username is not a valid username: {e}"))
        }
    }
}

pub(crate) fn extract_code_block(body: &[&str]) -> Result<Vec<String>, String> {
    if body.len() < 3
        || body[0].trim() != "```"
        || body.last().unwrap().trim() != "```"
    {
        return Err("Expected code block in command body. Add --help for \
                    more details."
            .to_owned());
    }
    let code_block: Vec<&str> = body[1..body.len() - 1].to_vec();
    let code_block_owned: Vec<String> =
        code_block.iter().map(|x| x.to_string()).collect();
    Ok(code_block_owned)
}
