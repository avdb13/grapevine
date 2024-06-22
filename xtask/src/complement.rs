use std::{
    fs::{self},
    path::{Path, PathBuf},
};

use miette::{miette, IntoDiagnostic, LabeledSpan, Result, WrapErr};
use serde::Deserialize;
use xshell::{cmd, Shell};

mod docker;
mod summary;
mod test2json;

use self::{
    docker::{load_docker_image, retag_docker_image},
    summary::{compare_summary, read_summary},
    test2json::{count_complement_tests, run_complement},
};

/// Runs complement tests, writes results to an output directory, and compares
/// results with a baseline.
///
/// The output directory structure is
///
///  - `$out/summary.tsv`: a TSV file with the pass/fail/skip result for each
///    test
///
///  - `$out/raw-log.jsonl`: raw output of the go test2json tool
///
///  - `$out/logs/...`: a text file named `$test.txt` for each test, containing
///    the test logs.
///
/// These files will be updated incrementally during the test run. When the run
/// the complete, the wrapper compares the results in `$out/summary.tsv`
/// against the baseline result. If there are any differences, it exits with an
/// error.
///
/// The expected workflow is to run this after making changes to Grapevine, to
/// look for regressions in tests that were previously passing. If you make
/// change that fix an existing failing test, you need to make sure that they
/// did not introduce any regressions, and then copy the `summary.tsv` file from
/// your test run over the existing `complement-baseline.tsv` file in the
/// repository root. The intent is that `complement-baseline.tsv` should always
/// be in sync with the expected results from a test run.
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
pub(crate) fn main(args: Args, sh: &Shell) -> Result<()> {
    let toplevel = get_toplevel_path(sh)
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
    let docker_image = load_docker_image(sh, &toplevel).wrap_err(
        "failed to build and load complement-grapevine docker image",
    )?;
    let docker_image = retag_docker_image(sh, &docker_image)
        .wrap_err("failed to retag docker image")?;
    let test_count = count_complement_tests(sh, &docker_image)
        .wrap_err("failed to determine total complement test count")?;
    let results = run_complement(sh, &args.out, &docker_image, test_count)
        .wrap_err("failed to run complement tests")?;
    let summary_path = args.out.join("summary.tsv");
    compare_summary(&baseline, &results, &baseline_path, &summary_path)?;
    println!("\nTest results were identical to baseline.");
    Ok(())
}

/// Deserialize a single-line json string using [`serde_json::from_str`] and
/// convert the error to a miette diagnostic.
///
/// # Panics
/// Panics if `line` contains a newline.
fn from_json_line<'a, T: Deserialize<'a>>(line: &'a str) -> Result<T> {
    assert!(
        !line.contains('\n'),
        "from_json_line requires single-line json source"
    );
    serde_json::from_str(line).map_err(|e| {
        // Needs single-line input so that we don't have to deal with converting
        // line/column to a span offset.
        let offset = e.column() - 1;
        let label = LabeledSpan::at_offset(offset, "error here");
        miette!(labels = vec![label], "{e}").with_source_code(line.to_owned())
    })
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
