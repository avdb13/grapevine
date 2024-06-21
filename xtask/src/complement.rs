use std::path::PathBuf;

use miette::{IntoDiagnostic, Result, WrapErr};
use xshell::{cmd, Shell};

mod docker;
mod test2json;

use self::{docker::load_docker_image, test2json::run_complement};

#[derive(clap::Args)]
pub(crate) struct Args;

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn main(_args: Args) -> Result<()> {
    let sh = Shell::new().unwrap();
    let toplevel = get_toplevel_path(&sh)
        .wrap_err("failed to determine repository root directory")?;
    let docker_image = load_docker_image(&sh, &toplevel).wrap_err(
        "failed to build and load complement-grapevine docker image",
    )?;
    run_complement(&sh, &docker_image)
        .wrap_err("failed to run complement tests")?;
    Ok(())
}

/// Returns the path to the repository root
fn get_toplevel_path(sh: &Shell) -> Result<PathBuf> {
    let path =
        cmd!(sh, "git rev-parse --show-toplevel").read().into_diagnostic()?;
    Ok(path.into())
}
