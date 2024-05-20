use crate::services;

pub(crate) fn try_process() -> Result<String, String> {
    match services().users.list_local_users() {
        Ok(users) => {
            let mut msg: String =
                format!("Found {} local user account(s):\n", users.len());
            msg += &users.join("\n");
            Ok(msg.clone())
        }
        Err(e) => Err(e.to_string())
    }
}
