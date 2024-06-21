//! Functions for working with the go [`test2json`][test2json] tool.
//!
//! [test2json]: https://pkg.go.dev/cmd/test2json@go1.22.4

use miette::{IntoDiagnostic, Result};
use xshell::{cmd, Shell};

/// Runs complement test suite
pub(crate) fn run_complement(sh: &Shell, docker_image: &str) -> Result<()> {
    // TODO: handle SIG{INT,TERM}
    // TODO: XTASK_PATH variable, so that we don't need to pollute devshell with
    // go
    cmd!(sh, "go tool test2json complement.test -test.v=test2json")
        .env("COMPLEMENT_BASE_IMAGE", docker_image)
        .env("COMPLEMENT_SPAWN_HS_TIMEOUT", "5")
        .env("COMPLEMENT_ALWAYS_PRINT_SERVER_LOGS", "1")
        .run()
        .into_diagnostic()
}
