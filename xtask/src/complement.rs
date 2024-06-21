use std::{
    fs::{self},
    path::{Path, PathBuf},
};

use miette::{miette, IntoDiagnostic, Result, WrapErr};
use xshell::{cmd, Shell};

mod docker;
mod summary;
mod test2json;

use self::{
    docker::load_docker_image,
    summary::{compare_summary, read_summary},
    test2json::{count_complement_tests, run_complement},
};

#[derive(clap::Args)]
pub(crate) struct Args {
    /// Directory to write test results
    ///
    /// This directory will be created automatically, but it must be empty.
    /// If it exists and is not empty, an error will be returned.
    #[clap(short, long)]
    out: PathBuf,

    /// Baseline test summary file to compare with
    ///
    /// If unspecified, defaults to `$repo_root/complement-baseline.tsv`
    #[clap(short, long)]
    baseline: Option<PathBuf>,
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn main(args: Args) -> Result<()> {
    let sh = Shell::new().unwrap();
    let toplevel = get_toplevel_path(&sh)
        .wrap_err("failed to determine repository root directory")?;
    let baseline_path = args
        .baseline
        .unwrap_or_else(|| toplevel.join("complement-baseline.tsv"));
    let baseline = read_summary(&baseline_path).wrap_err_with(|| {
        format!(
            "failed to read baseline test result summary from \
             {baseline_path:?}"
        )
    })?;
    create_out_dir(&args.out).wrap_err_with(|| {
        format!("error initializing output directory {:?}", args.out)
    })?;
    let docker_image = load_docker_image(&sh, &toplevel).wrap_err(
        "failed to build and load complement-grapevine docker image",
    )?;
    let test_count = count_complement_tests(&sh, &docker_image)
        .wrap_err("failed to determine total complement test count")?;
    let results = run_complement(&sh, &args.out, &docker_image, test_count)
        .wrap_err("failed to run complement tests")?;
    let summary_path = args.out.join("summary.tsv");
    compare_summary(&baseline, &results, &baseline_path, &summary_path)?;
    println!("\nTest results were identical to baseline.");
    Ok(())
}

/// Ensures that output directory exists and is empty
///
/// If the directory does not exist, it will be created. If it is not empty, an
/// error will be returned.
///
/// We have no protection against concurrent programs modifying the contents of
/// the directory while the complement wrapper tool is running.
fn create_out_dir(out: &Path) -> Result<()> {
    fs::create_dir_all(out)
        .into_diagnostic()
        .wrap_err("error creating output directory")?;
    let mut entries = fs::read_dir(out)
        .into_diagnostic()
        .wrap_err("error checking current contents of output directory")?;
    if entries.next().is_some() {
        return Err(miette!(
            "output directory is not empty. Refusing to run, instead of \
             possibly overwriting existing files."
        ));
    }
    fs::create_dir(out.join("logs"))
        .into_diagnostic()
        .wrap_err("error creating logs subdirectory in output directory")?;
    Ok(())
}

/// Returns the path to the repository root
fn get_toplevel_path(sh: &Shell) -> Result<PathBuf> {
    let path =
        cmd!(sh, "git rev-parse --show-toplevel").read().into_diagnostic()?;
    Ok(path.into())
}
