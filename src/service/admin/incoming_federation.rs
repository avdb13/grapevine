use crate::services;

use std::fmt::Write;

pub(crate) async fn try_process() -> Result<String, String> {
    let map = services().globals.roomid_federationhandletime.read().await;
    let mut msg: String = format!("Handling {} incoming pdus:\n", map.len());

    for (r, (e, i)) in map.iter() {
        let elapsed = i.elapsed();
        writeln!(
            msg,
            "{r} {e}: {elapsed:?}"
        )
        .expect("write to in-memory buffer should succeed");
    }
    Ok(msg)
}
