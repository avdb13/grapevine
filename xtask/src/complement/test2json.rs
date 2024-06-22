//! Functions for working with the go [`test2json`][test2json] tool.
//!
//! [test2json]: https://pkg.go.dev/cmd/test2json@go1.22.4

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Seek, SeekFrom, Write},
    mem, panic,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

use indicatif::{ProgressBar, ProgressStyle};
use miette::{miette, IntoDiagnostic, LabeledSpan, Result, WrapErr};
use process_wrap::std::{ProcessGroup, StdChildWrapper, StdCommandWrap};
use serde::Deserialize;
use signal_hook::{
    consts::signal::{SIGINT, SIGQUIT, SIGTERM},
    flag,
    iterator::Signals,
};
use strum::{Display, EnumString};
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

/// Run complement tests.
///
/// This function mostly deals with handling shutdown signals, while the actual
/// logic for running complement is in `run_complement_inner`, which is spawned
/// as a separate thread. This is necessary because the go `test2json` tool
/// ignores SIGTERM and SIGINT. Without signal handling on our end, terminating
/// the complement wrapper process would leave a dangling complement child
/// process running.
///
/// The reason that `test2json` does this is that it does not implement any kind
/// of test cleanup, and so the developers decided that ignoring termination
/// signals entirely was safer. Running go unit tests outside of `test2json`
/// (and so without machine-readable output) does not have this limitation.
/// Unfortunately neither of these are an option for us. We need
/// machine-readable output to compare against the baseline result. Complement
/// runs can take 40+ minutes, so being able to cancel them is a requirement.
///
/// Because we don't trigger any of the normal cleanup, we need to handle
/// dangling docker containers ourselves.
pub(crate) fn run_complement(
    sh: &Shell,
    out: &Path,
    docker_image: &str,
    test_count: u64,
) -> Result<TestResults> {
    let term_signals = [SIGTERM, SIGINT, SIGQUIT];

    let term_now = Arc::new(AtomicBool::new(false));
    for sig in &term_signals {
        // Terminate immediately if `term_now` is true and we receive a
        // terminating signal
        flag::register_conditional_shutdown(*sig, 1, Arc::clone(&term_now))
            .into_diagnostic()
            .wrap_err("error registering signal handler")?;
    }

    let mut signals = Signals::new(term_signals).unwrap();

    let state = Mutex::new(ComplementRunnerState::Startup);
    let signals_handle = signals.handle();

    let result = thread::scope(|s| {
        let state_ref = &state;
        let cloned_sh = sh.clone();
        let thread_handle = s.spawn(move || {
            let panic_result = panic::catch_unwind(|| {
                run_complement_inner(
                    &cloned_sh,
                    out,
                    docker_image,
                    test_count,
                    state_ref,
                )
            });
            // Stop the signal-handling loop, even if we panicked
            signals_handle.close();
            match panic_result {
                Ok(result) => result,
                Err(panic) => panic::resume_unwind(panic),
            }
        });

        let canceled = if let Some(signal) = signals.forever().next() {
            let description = match signal {
                SIGTERM => "SIGTERM",
                SIGINT => "ctrl+c",
                SIGQUIT => "SIGQUIT",
                _ => unreachable!(),
            };
            eprintln!(
                "Received {description}, stopping complement run. Send \
                 {description} a second time to terminate without cleaning \
                 up, which may leave dangling processes and docker containers"
            );
            term_now.store(true, Ordering::Relaxed);

            {
                let mut state = state.lock().unwrap();
                let old_state =
                    mem::replace(&mut *state, ComplementRunnerState::Shutdown);
                match old_state {
                    ComplementRunnerState::Startup => (),
                    ComplementRunnerState::Shutdown => unreachable!(),
                    ComplementRunnerState::Running(mut child) => {
                        // Killing the child process should terminate the
                        // complement runner thread in a
                        // bounded amount of time, because it will cause the
                        // stdout reader to return EOF.
                        child.kill().unwrap();
                    }
                }
            }

            // TODO: kill dangling docker containers
            eprintln!(
                "WARNING: complement may have left dangling docker \
                 containers. Cleanup for these is planned, but has not been \
                 implemented yet. You need to identify and kill them manually"
            );

            true
        } else {
            // hit this branch if the signal handler is closed by the complement
            // runner thread. This means the complement run finished
            // without being canceled.
            false
        };

        match thread_handle.join() {
            Ok(result) => {
                if canceled {
                    Err(miette!("complement run was canceled"))
                } else {
                    result
                }
            }
            Err(panic_value) => panic::resume_unwind(panic_value),
        }
    });

    // From this point on, terminate immediately when signalled
    term_now.store(true, Ordering::Relaxed);

    result
}

/// Possible states for the complement runner thread.
///
/// The current state should be protected by a mutex, where state changes are
/// only performed while the mutex is locked. This is to prevent a race
/// condition where the main thread handles a shutdown signal at the same time
/// that the complement runner thread is starting the child process, and so the
/// main thread fails to kill the child process.
///
/// Valid state transitions:
///
///  - `Startup` -> `Running`
///  - `Startup` -> `Shutdown`
///  - `Running` -> `Shutdown`
#[derive(Debug)]
enum ComplementRunnerState {
    /// The complement child process has not been started yet
    Startup,
    /// The complement child process is running, and we have not yet received
    /// a shutdown signal.
    Running(Box<dyn StdChildWrapper>),
    /// We have received a shutdown signal.
    Shutdown,
}

/// Spawn complement chind process and handle it's output
///
/// This is the "complement runner" thread, spawned by the [`run_complement`]
/// function.
fn run_complement_inner(
    sh: &Shell,
    out: &Path,
    docker_image: &str,
    test_count: u64,
    state: &Mutex<ComplementRunnerState>,
) -> Result<TestResults> {
    let cmd = cmd!(sh, "go tool test2json complement.test -test.v=test2json")
        .env("COMPLEMENT_BASE_IMAGE", docker_image)
        .env("COMPLEMENT_SPAWN_HS_TIMEOUT", "5")
        .env("COMPLEMENT_ALWAYS_PRINT_SERVER_LOGS", "1");
    eprintln!("$ {cmd}");

    let stdout = {
        let mut state = state.lock().unwrap();
        match &*state {
            ComplementRunnerState::Startup => (),
            ComplementRunnerState::Running(_) => unreachable!(),
            ComplementRunnerState::Shutdown => {
                return Err(miette!("complement run was canceled"))
            }
        }
        let mut cmd = Command::from(cmd);
        cmd.stdout(Stdio::piped());
        let mut child = StdCommandWrap::from(cmd)
            .wrap(ProcessGroup::leader())
            .spawn()
            .into_diagnostic()
            .wrap_err("error spawning complement process")?;
        let stdout = child.stdout().take().expect(
            "child process spawned with piped stdout should have stdout",
        );
        *state = ComplementRunnerState::Running(child);
        stdout
    };
    let lines = BufReader::new(stdout).lines();

    let mut ctx = TestContext::new(out, test_count)?;
    for line in lines {
        let line = line
            .into_diagnostic()
            .wrap_err("error reading output from complement process")?;
        ctx.handle_line(&line)?;
    }

    Ok(ctx.results)
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

#[derive(Copy, Clone, Display, EnumString, Eq, PartialEq, Debug)]
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
