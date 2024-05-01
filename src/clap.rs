//! Integration with `clap`

use clap::Parser;

/// Command line arguments
#[derive(Parser)]
#[clap(about, version = crate::version())]
pub(crate) struct Args;

/// Parse command line arguments into structured data
pub(crate) fn parse() -> Args {
    Args::parse()
}
