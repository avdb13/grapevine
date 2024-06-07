//! Integration with `clap`

use std::path::PathBuf;

use clap::Parser;

/// Command line arguments
#[derive(Parser)]
#[clap(about, version = crate::version())]
pub(crate) struct Args {
    /// Path to the configuration file
    #[clap(long, short)]
    pub(crate) config: PathBuf,
}

/// Parse command line arguments into structured data
pub(crate) fn parse() -> Args {
    Args::parse()
}
