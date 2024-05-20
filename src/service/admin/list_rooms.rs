use crate::services;

// Clippy: This function seems infallible but we still need to wrap it in a Result<String, String> to preserve the return type for matching argv[1]
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn try_process() -> Result<String, String> {
    let room_ids = services().rooms.metadata.iter_ids();
    let output = format!(
        "Rooms:\n{}",
        room_ids
            .filter_map(std::result::Result::ok)
            .map(|id| format!(
                "{id}\tMembers: {}",
                &services()
                    .rooms
                    .state_cache
                    .room_joined_count(&id)
                    .ok()
                    .flatten()
                    .unwrap_or(0)
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
    Ok(output)
}
