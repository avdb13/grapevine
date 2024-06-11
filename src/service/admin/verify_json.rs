use std::collections::BTreeMap;

use tokio::sync::RwLock;

use crate::{service::admin::common::extract_code_block, services};

pub(crate) async fn try_process(body: Vec<&str>) -> Result<String, String> {
    let string = extract_code_block(&body)?.join("\n");
    match serde_json::from_str(&string) {
        Ok(value) => {
            let pub_key_map = RwLock::new(BTreeMap::new());

            if let Err(e) = services()
                .rooms
                .event_handler
                .fetch_required_signing_keys(&value, &pub_key_map)
                .await
            {
                return Err(format!(
                    "Error fetching required signing keys {e:?}"
                ));
            }

            let pub_key_map = pub_key_map.read().await;
            match ruma::signatures::verify_json(&pub_key_map, &value) {
                Ok(()) => Ok("Signature correct".to_owned()),
                Err(e) => Err(format!("Signature verification failed: {e}")),
            }
        }
        Err(e) => Err(format!("Invalid json: {e}")),
    }
}
