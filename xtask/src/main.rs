mod complement;

use std::{env, ffi::OsString, process::ExitCode};

use clap::{Parser, Subcommand};
use miette::{miette, IntoDiagnostic, Result, WrapErr};
use xshell::Shell;

#[derive(Parser)]
struct Args {
    #[clap(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Complement(complement::Args),
}

fn main() -> ExitCode {
    let Err(e) = try_main() else {
        return ExitCode::SUCCESS;
    };
    // Include a leading newline because sometimes an error will occur in
    // the middle of displaying a progress indicator.
    eprintln!("\n{e:?}");
    ExitCode::FAILURE
}

fn try_main() -> Result<()> {
    let args = Args::parse();
    let sh = new_shell()?;
    match args.command {
        Command::Complement(args) => complement::main(args, &sh),
    }
}

fn new_shell() -> Result<Shell> {
    let path = get_shell_path()?;
    let sh = Shell::new()
        .into_diagnostic()
        .wrap_err("failed to initialize internal xshell::Shell wrapper")?;
    sh.set_var("PATH", path);
    Ok(sh)
}

/// Returns the value to set the `PATH` environment variable to in
/// [`xshell::Shell`] instances.
///
/// This function appends the paths from the `GRAPEVINE_XTASK_PATH` environment
/// variable to the existing value of `PATH` set in the xtask process.
///
/// Executable dependencies that are only called by commands in xtask should be
/// added to `GRAPEVINE_XTASK_PATH` instead of `PATH` in the devshell, to avoid
/// polluting the devshell path with extra entries.
fn get_shell_path() -> Result<OsString> {
    let xtask_path = env::var_os("GRAPEVINE_XTASK_PATH").ok_or(miette!(
        help = "This tool must be run from inside the Grapevine devshell. \
                Make sure you didn't interrupt direnv or something similar.",
        "GRAPEVINE_XTASK_PATH environment variable is unset"
    ))?;
    if let Some(path) = env::var_os("PATH") {
        let old_paths = env::split_paths(&path);
        let xtask_paths = env::split_paths(&xtask_path);
        env::join_paths(old_paths.chain(xtask_paths))
            .into_diagnostic()
            .wrap_err(
                "error constructing new PATH value to include the paths from \
                 GRAPEVINE_XTASK_PATH",
            )
    } else {
        Ok(xtask_path)
    }
}
