//! Functions for working with the `summary.tsv` files emitted by the complement
//! wrapper.
//!
//! This file is a TSV containing test names and results for each test in a
//! complement run.

use std::{
    collections::BTreeMap,
    io::{BufWriter, Write},
};

use miette::{IntoDiagnostic, Result};

use super::test2json::TestResult;

pub(crate) type TestResults = BTreeMap<String, TestResult>;

/// Escape a string value for use in a TSV file.
///
/// According to the [tsv spec][1], the only characters that need to be escaped
/// are `\n`, `\t`, `\r`, and `\`.
///
/// [1]: https://www.loc.gov/preservation/digital/formats/fdd/fdd000533.shtml
fn escape_tsv_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
}

/// Write a test result summary to a writer.
pub(crate) fn write_summary<W: Write>(
    w: &mut BufWriter<W>,
    summary: &TestResults,
) -> Result<()> {
    // Write header line
    writeln!(w, "test\tresult").into_diagnostic()?;
    // Write rows
    for (test, result) in summary {
        writeln!(
            w,
            "{}\t{}",
            escape_tsv_value(test),
            escape_tsv_value(&result.to_string())
        )
        .into_diagnostic()?;
    }
    Ok(())
}
