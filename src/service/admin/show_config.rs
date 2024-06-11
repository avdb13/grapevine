use crate::services;

// Clippy: This function seems infallible, but we still need to wrap it in a
// Result<String, String> to preserve the return type for matching argv[1]
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn try_process() -> Result<String, String> {
    Ok(services().globals.config.to_string())
}
