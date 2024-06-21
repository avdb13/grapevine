//! Functions for working with docker images and containers.

use std::path::Path;

use miette::{miette, IntoDiagnostic, LabeledSpan, Result, WrapErr};
use xshell::{cmd, Shell};

/// Build the 'grapevine-complement' OCI image and load it into the docker
/// daemon.
pub(crate) fn load_docker_image(sh: &Shell, toplevel: &Path) -> Result<String> {
    // > i would Not trust that parser as far as i can throw it
    // - @jade_:matrix.org, 2024-06-19
    //
    // So we're not even gonna try to escape the arbitrary top level path
    // correctly for a flake installable reference. Instead we're just gonna cd
    // into toplevel before running nix commands.
    let _pushd_guard = sh.push_dir(toplevel);

    let installable = ".#complement-grapevine-oci-image";
    cmd!(sh, "nix-build-and-cache just {installable} -- --no-link")
        .run()
        .into_diagnostic()
        .wrap_err("error building complement-grapevine-oci-image")?;
    let oci_image_path = cmd!(sh, "nix path-info {installable}")
        .read()
        .into_diagnostic()
        .wrap_err(
            "error getting nix store path for complement-grapevine-oci-image",
        )?;

    // Instead of building the image with a fixed tag, we let nix choose the tag
    // based on the input hash, and then determine the image/tag it used by
    // parsing the 'docker load' output. This is to avoid a race condition
    // between multiple concurrent 'xtask complement' invocations, which might
    // otherwise assign the same tag to different images.
    let load_output = cmd!(sh, "docker image load --input {oci_image_path}")
        .read()
        .into_diagnostic()
        .wrap_err("error loading complement-grapevine docker image")?;
    let expected_prefix = "Loaded image: ";
    let docker_image = load_output
        .strip_prefix(expected_prefix)
        .ok_or_else(|| {
            // Miette doesn't support inclusive ranges.
            // <https://github.com/zkat/miette/pull/385>
            #[allow(clippy::range_plus_one)]
            let span = 0..(expected_prefix.len().min(load_output.len()) + 1);
            let label =
                LabeledSpan::at(span, format!("Expected {expected_prefix:?}"));
            miette!(
                labels = vec![label],
                "failed to parse 'docker image load' output"
            )
            .with_source_code(load_output.clone())
        })?
        .to_owned();
    Ok(docker_image)
}
