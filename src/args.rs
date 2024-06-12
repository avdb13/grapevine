//! Integration with `clap`

use std::path::PathBuf;

use clap::{CommandFactory as _, FromArgMatches as _, Parser};

/// Command line arguments
#[derive(Parser)]
#[clap(about, version = crate::version())]
pub(crate) struct Args {
    /// Path to the configuration file
    #[clap(long, short)]
    pub(crate) config: Option<PathBuf>,
}

/// Parse command line arguments into structured data
pub(crate) fn parse() -> Args {
    let mut command = Args::command().mut_arg("config", |x| {
        let help = "Set the path to the configuration file";
        x.help(help).long_help(format!(
            "{}\n\nIf this option is specified, the provided value is used \
             as-is.\n\nIf this option is not specified, then the XDG Base \
             Directory Specification is followed, searching for the path `{}` \
             in the configuration directories.
            ",
            help,
            crate::config::DEFAULT_PATH.display(),
        ))
    });

    match Args::from_arg_matches(&command.get_matches_mut()) {
        Ok(x) => x,
        Err(e) => e.format(&mut command).exit(),
    }
}
