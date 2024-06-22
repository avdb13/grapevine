//! Functions for working with docker images and containers.

use std::path::Path;

use miette::{miette, IntoDiagnostic, LabeledSpan, Result, WrapErr};
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use serde::Deserialize;
use xshell::{cmd, Shell};

use super::from_json_line;

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

/// Retags the docker image with a random tag. Returns the new image reference.
///
/// This is useful so that we can uniquely identify the set of docker containers
/// spawned by a complement run. Without using a unique tag, there is no way to
/// determine which docker containers to kill if a run is cancelled, since other
/// concurrent complement runs may have created containers with the same image.
pub(crate) fn retag_docker_image(sh: &Shell, image: &str) -> Result<String> {
    let mut rng = thread_rng();
    let new_tag: String =
        (0..16).map(|_| char::from(rng.sample(Alphanumeric))).collect();
    let (repo, _old_tag) = image.split_once(':').ok_or_else(|| {
        miette!(
            "Docker image reference was not in the expected format. Expected \
             \"{{repository}}:{{tag}}\", got {image:?}"
        )
    })?;
    let new_image = format!("{repo}:{new_tag}");
    cmd!(sh, "docker image tag {image} {new_image}").run().into_diagnostic()?;
    Ok(new_image)
}

/// Kills all docker containers using a particular image.
///
/// This can be used to clean up dangling docker images after a cancelled
/// complement run, but it's important that the image reference be unique. See
/// the [`retag_docker_image`] function for a discussion of this.
pub(crate) fn kill_docker_containers(sh: &Shell, image: &str) -> Result<()> {
    #[derive(Deserialize)]
    struct ContainerInfo {
        #[serde(rename = "ID")]
        id: String,
        #[serde(rename = "Image")]
        image: String,
    }

    // --filter ancestor={image} doesn't work here, because images with the same
    // image id will be picked up even if their image reference (repo:tag) are
    // different. We need to list all the containers and filter them ourselves.
    let containers = cmd!(sh, "docker container ls --format json")
        .read()
        .into_diagnostic()
        .wrap_err("error listing running docker containers")?;
    let containers = containers
        .lines()
        .map(from_json_line)
        .collect::<Result<Vec<ContainerInfo>, _>>()
        .wrap_err(
            "error parsing docker container info from 'docker container ls' \
             output",
        )?;

    let our_containers = containers
        .into_iter()
        .filter(|container| container.image == image)
        .map(|container| container.id)
        .collect::<Vec<_>>();

    if !our_containers.is_empty() {
        // Ignore non-zero exit status because 'docker kill' will fail if
        // containers already exited before sending the signal, which is
        // fine.
        cmd!(sh, "docker kill --signal=SIGKILL {our_containers...}")
            .ignore_status()
            .run()
            .into_diagnostic()
            .wrap_err("error killing docker containers")?;
    }

    Ok(())
}
