//! Functions for working with the go [`test2json`][test2json] tool.
//!
//! [test2json]: https://pkg.go.dev/cmd/test2json@go1.22.4

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use indicatif::{ProgressBar, ProgressStyle};
use miette::{miette, IntoDiagnostic, LabeledSpan, Result, WrapErr};
use serde::Deserialize;
use strum::Display;
use xshell::{cmd, Shell};

use super::summary::{write_summary, TestResults};

/// Returns the total number of complement tests that will be run
///
/// This is only able to count toplevel tests, and will not included subtests
/// (`A/B`)
pub(crate) fn count_complement_tests(
    sh: &Shell,
    docker_image: &str,
) -> Result<u64> {
    let test_list = cmd!(sh, "go tool test2json complement.test -test.list .*")
        .env("COMPLEMENT_BASE_IMAGE", docker_image)
        .read()
        .into_diagnostic()?;
    let test_count = u64::try_from(test_list.lines().count())
        .into_diagnostic()
        .wrap_err("test count overflowed u64")?;
    Ok(test_count)
}

/// Runs complement test suite
pub(crate) fn run_complement(
    sh: &Shell,
    out: &Path,
    docker_image: &str,
    test_count: u64,
) -> Result<()> {
    // TODO: handle SIG{INT,TERM}
    // TODO: XTASK_PATH variable, so that we don't need to pollute devshell with
    // go
    let cmd = cmd!(sh, "go tool test2json complement.test -test.v=test2json")
        .env("COMPLEMENT_BASE_IMAGE", docker_image)
        .env("COMPLEMENT_SPAWN_HS_TIMEOUT", "5")
        .env("COMPLEMENT_ALWAYS_PRINT_SERVER_LOGS", "1");
    eprintln!("$ {cmd}");
    let child = Command::from(cmd)
        .stdout(Stdio::piped())
        .spawn()
        .into_diagnostic()
        .wrap_err("error spawning complement process")?;
    let stdout = child
        .stdout
        .expect("child process spawned with piped stdout should have stdout");
    let lines = BufReader::new(stdout).lines();

    let mut ctx = TestContext::new(out, test_count)?;
    for line in lines {
        let line = line
            .into_diagnostic()
            .wrap_err("error reading output from complement process")?;
        ctx.handle_line(&line)?;
    }

    Ok(())
}

/// Schema from <https://pkg.go.dev/cmd/test2json#hdr-Output_Format>
///
/// Only the fields that we need are included here.
#[derive(Deserialize)]
#[serde(
    rename_all = "snake_case",
    rename_all_fields = "PascalCase",
    tag = "Action"
)]
enum GoTestEvent {
    Run {
        test: Option<String>,
    },
    Pass {
        test: Option<String>,
    },
    Fail {
        test: Option<String>,
    },
    Skip {
        test: Option<String>,
    },
    Output {
        test: Option<String>,
        output: String,
    },
    #[serde(other)]
    OtherAction,
}

#[derive(Copy, Clone, Display, Debug)]
#[strum(serialize_all = "UPPERCASE")]
pub(crate) enum TestResult {
    Pass,
    Fail,
    Skip,
}

struct TestContext {
    pb: ProgressBar,
    pass_count: u64,
    fail_count: u64,
    skip_count: u64,
    // We do not need a specific method to flush this before dropping
    // `TestContext`, because the file is only written from the
    // `update_summary_file` method. This method always calls flush on
    // a non-error path, and the file is left in an inconsistent state on an
    // error anyway.
    summary_file: BufWriter<File>,
    log_dir: PathBuf,
    results: TestResults,
}

/// Returns a string to use for displaying a test name
///
/// From the test2json docs:
///
/// > The Test field, if present, specifies the test, example, or benchmark
/// > function that caused the event. Events for the overall package test do not
/// > set Test.
///
/// For events that do not have a `Test` field, we display their test name as
/// `"GLOBAL"` instead.
fn test_str(test: &Option<String>) -> &str {
    if let Some(test) = test {
        test
    } else {
        "GLOBAL"
    }
}

/// Returns whether a test name is a toplevel test (as opposed to a subtest)
fn test_is_toplevel(test: &str) -> bool {
    !test.contains('/')
}

impl TestContext {
    fn new(out: &Path, test_count: u64) -> Result<TestContext> {
        // TODO: figure out how to display ETA without it fluctuating wildly.
        let style = ProgressStyle::with_template(
            "({msg})  {pos}/{len}  [{elapsed}] {wide_bar}",
        )
        .expect("static progress bar template should be valid")
        .progress_chars("##-");
        let pb = ProgressBar::new(test_count).with_style(style);
        pb.enable_steady_tick(Duration::from_secs(1));

        let summary_file = File::create(out.join("summary.tsv"))
            .into_diagnostic()
            .wrap_err("failed to create summary file in output dir")?;
        let summary_file = BufWriter::new(summary_file);

        let log_dir = out.join("logs");

        let ctx = TestContext {
            pb,
            pass_count: 0,
            fail_count: 0,
            skip_count: 0,
            log_dir,
            summary_file,
            results: BTreeMap::new(),
        };

        ctx.update_progress();
        Ok(ctx)
    }

    fn update_progress(&self) {
        self.pb
            .set_position(self.pass_count + self.fail_count + self.skip_count);
        self.pb.set_message(format!(
            "PASS {}, FAIL {}, SKIP {}",
            self.pass_count, self.fail_count, self.skip_count
        ));
    }

    fn update_summary_file(&mut self) -> Result<()> {
        // Truncate the file to clear existing contents
        self.summary_file
            .get_mut()
            .seek(SeekFrom::Start(0))
            .into_diagnostic()?;
        self.summary_file.get_mut().set_len(0).into_diagnostic()?;
        write_summary(&mut self.summary_file, &self.results)?;
        self.summary_file.flush().into_diagnostic()?;
        Ok(())
    }

    fn handle_test_result(
        &mut self,
        test: &str,
        result: TestResult,
    ) -> Result<()> {
        self.pb.println(format!("=== {result}\t{test}"));
        self.results.insert(test.to_owned(), result);
        // 'complement.test -test.list' is only able to count toplevel tests
        // ahead-of-time, so we don't include subtests in the pass/fail/skip
        // counts.
        if test_is_toplevel(test) {
            match result {
                TestResult::Pass => self.pass_count += 1,
                TestResult::Fail => self.fail_count += 1,
                TestResult::Skip => self.skip_count += 1,
            }
            self.update_progress();
        }
        self.update_summary_file().wrap_err("error writing summary file")?;
        Ok(())
    }

    fn handle_test_output(&mut self, test: &str, output: &str) -> Result<()> {
        let path = self.log_dir.join(test).with_extension("txt");

        // Some tests have a '/' in their name, so create the extra dirs if they
        // don't already exist.
        let parent_dir = path.parent().expect(
            "log file path should have parent. At worst, the toplevel dir is \
             $out/logs/.",
        );
        fs::create_dir_all(parent_dir).into_diagnostic().wrap_err_with(
            || {
                format!(
                    "error creating directory at {parent_dir:?} for log file \
                     {path:?}"
                )
            },
        )?;

        let mut log_file = File::options()
            .create(true)
            .append(true)
            .open(&path)
            .into_diagnostic()
            .wrap_err_with(|| format!("error creating log file at {path:?}"))?;
        log_file.write_all(output.as_bytes()).into_diagnostic().wrap_err_with(
            || format!("error writing to log file at {path:?}"),
        )?;
        Ok(())
    }

    fn handle_event(&mut self, event: GoTestEvent) -> Result<()> {
        match event {
            GoTestEvent::OtherAction => (),
            GoTestEvent::Run {
                test,
            } => {
                self.pb.println(format!("=== RUN \t{}", test_str(&test)));
            }
            GoTestEvent::Pass {
                test,
            } => {
                self.handle_test_result(test_str(&test), TestResult::Pass)?;
            }
            GoTestEvent::Fail {
                test,
            } => {
                self.handle_test_result(test_str(&test), TestResult::Fail)?;
            }
            GoTestEvent::Skip {
                test,
            } => {
                self.handle_test_result(test_str(&test), TestResult::Skip)?;
            }
            GoTestEvent::Output {
                test,
                output,
            } => {
                let test = test_str(&test);
                self.handle_test_output(test, &output).wrap_err_with(|| {
                    format!(
                        "failed to write test output to a log file for test \
                         {test:?}"
                    )
                })?;
            }
        }
        Ok(())
    }

    /// Processes a line of output from `test2json`
    fn handle_line(&mut self, line: &str) -> Result<()> {
        match serde_json::from_str(line) {
            Ok(event) => self.handle_event(event)?,
            Err(e) => {
                let label =
                    LabeledSpan::at_offset(e.column() - 1, "error here");
                let report = miette!(labels = vec![label], "{e}",)
                    .with_source_code(line.to_owned())
                    .wrap_err(
                        "failed to parse go test2json event from complement \
                         tests. Ignoring this event.",
                    );
                eprintln!("{report:?}");
            }
        };
        Ok(())
    }
}
