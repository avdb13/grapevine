//! Functions for working with the `summary.tsv` files emitted by the complement
//! wrapper.
//!
//! This file is a TSV containing test names and results for each test in a
//! complement run.

use std::{
    collections::BTreeMap,
    fs,
    io::{BufWriter, Write},
    path::Path,
};

use miette::{
    miette, IntoDiagnostic, LabeledSpan, NamedSource, Result, WrapErr,
};

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

/// Converts a string from a TSV value from to unescaped form.
fn unescape_tsv_value(value: &str) -> String {
    let mut chars = value.chars();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some(c2) => {
                    out.push(c);
                    out.push(c2);
                }
                None => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
    out
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

/// Reads test result summary from a TSV file written by a previous run of the
/// complement wrapper.
pub(crate) fn read_summary(
    path: &Path,
) -> Result<BTreeMap<String, TestResult>> {
    let contents = fs::read_to_string(path)
        .into_diagnostic()
        .wrap_err("failed to read summary file contents")?;
    let source = NamedSource::new(path.to_string_lossy(), contents);
    let contents = &source.inner();

    let mut offset = 0;
    // The TSV spec allows CRLF, but we never emit these ourselves
    let mut lines = contents.split('\n');

    let header_line = lines.next().ok_or_else(|| {
        miette!(
            labels = vec![LabeledSpan::at_offset(0, "expected header row")],
            "summary file missing header row",
        )
        .with_source_code(source.clone())
    })?;
    let expected_header_line = "test\tresult";
    if header_line != expected_header_line {
        return Err(miette!(
            labels = vec![LabeledSpan::at(
                0..header_line.len(),
                "unexpected header"
            )],
            "summary file header row has unexpected columns. Expecting \
             {expected_header_line:?}."
        )
        .with_source_code(source));
    }
    offset += header_line.len() + 1;

    let mut results = BTreeMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }

        let tabs = line.match_indices('\t').collect::<Vec<_>>();
        let column_count = tabs.len() + 1;
        let (result_span, test, result) = match tabs[..] {
            [(first_tab, _)] => {
                let result_span = offset + first_tab + 1..offset + line.len();
                let test = line.get(..first_tab).expect(
                    "index should be valid because it was returned from \
                     'match_indices'",
                );
                let result = line.get(first_tab + 1..).expect(
                    "index should be valid because it was returned from \
                     'match_indices'",
                );
                (result_span, test, result)
            }
            [] => {
                return Err(miette!(
                    labels = vec![LabeledSpan::at_offset(
                        offset + line.len(),
                        "expected more columns here"
                    )],
                    "each row in the summary file should have exactly two \
                     columns. This row only has {column_count} columns.",
                )
                .with_source_code(source))
            }
            [_, (first_bad_tab, _), ..] => {
                let span = offset + first_bad_tab..offset + line.len();
                return Err(miette!(
                    labels =
                        vec![LabeledSpan::at(span, "unexpected extra columns")],
                    "each row in the summary file should have exactly two \
                     columns. This row has {column_count} columns.",
                )
                .with_source_code(source));
            }
        };

        let test = unescape_tsv_value(test);
        let result = unescape_tsv_value(result);

        let result = result.parse().map_err(|_| {
            miette!(
                labels =
                    vec![LabeledSpan::at(result_span, "invalid result value")],
                "test result value must be one of 'PASS', 'FAIL', or 'SKIP'."
            )
            .with_source_code(source.clone())
        })?;

        results.insert(test, result);
        offset += line.len() + 1;
    }
    Ok(results)
}

/// Print a bulleted list of test names, truncating if there are too many.
fn print_truncated_tests(tests: &[&str]) {
    let max = 5;
    for test in &tests[..max.min(tests.len())] {
        println!("    - {test}");
    }
    if tests.len() > max {
        println!("    ... ({} more)", tests.len() - max);
    }
}

/// Compares new test results against older results, returning a error if they
/// differ.
///
/// A description of the differences will be logged separately from the returned
/// error.
pub(crate) fn compare_summary(
    old: &TestResults,
    new: &TestResults,
    old_path: &Path,
    new_path: &Path,
) -> Result<()> {
    let mut unexpected_pass: Vec<&str> = Vec::new();
    let mut unexpected_fail: Vec<&str> = Vec::new();
    let mut unexpected_skip: Vec<&str> = Vec::new();
    let mut added: Vec<&str> = Vec::new();
    let mut removed: Vec<&str> = Vec::new();

    for (test, new_result) in new {
        if let Some(old_result) = old.get(test) {
            if old_result != new_result {
                match new_result {
                    TestResult::Pass => unexpected_pass.push(test),
                    TestResult::Fail => unexpected_fail.push(test),
                    TestResult::Skip => unexpected_skip.push(test),
                }
            }
        } else {
            added.push(test);
        }
    }
    for test in old.keys() {
        if !new.contains_key(test) {
            removed.push(test);
        }
    }

    let mut differences = false;
    if !added.is_empty() {
        differences = true;
        println!(
            "\n{} tests were added that were not present in the baseline:",
            added.len()
        );
        print_truncated_tests(&added);
    }
    if !removed.is_empty() {
        differences = true;
        println!(
            "\n{} tests present in the baseline were removed:",
            removed.len()
        );
        print_truncated_tests(&removed);
    }
    if !unexpected_pass.is_empty() {
        differences = true;
        println!(
            "\n{} tests passed that did not pass in the baseline:",
            unexpected_pass.len()
        );
        print_truncated_tests(&unexpected_pass);
    }
    if !unexpected_skip.is_empty() {
        differences = true;
        println!(
            "\n{} tests skipped that were not skipped in the baseline:",
            unexpected_skip.len()
        );
        print_truncated_tests(&unexpected_skip);
    }
    if !unexpected_fail.is_empty() {
        differences = true;
        println!(
            "\n{} tests failed that did not fail in the baseline (these are \
             likely regressions):",
            unexpected_fail.len()
        );
        print_truncated_tests(&unexpected_fail);
    }

    if differences {
        Err(miette!(
            help = format!(
                "Evaluate each of the differences to determine whether they \
                 are expected. If all differences are expected, copy the new \
                 summary file {new_path:?} to {old_path:?} and commit the \
                 change. If some differences are unexpected, fix them and try \
                 another test run.\n\nAn example of an expected change would \
                 be a test that is now passing after your changes fixed it. \
                 An example of an unexpected change would be an unrelated \
                 test that is now failing, which would be a regression."
            ),
            "Test results differed from baseline in {old_path:?}. The \
             differences are described above."
        ))
    } else {
        Ok(())
    }
}
