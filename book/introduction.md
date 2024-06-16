# Introduction

Grapevine is a [Matrix][matrix] homeserver that was originally forked from
[Conduit 0.7.0][conduit].

[matrix]: https://matrix.org/
[conduit]: https://gitlab.com/famedly/conduit/-/tree/v0.7.0?ref_type=tags

## Goals

Our goal is to provide a robust and reliable Matrix homeserver implementation.
In order to accomplish this, we aim to do the following:

* Optimize for maintainability
* Implement automated testing to ensure correctness
* Improve instrumentation to provide real-world data to aid decision-making

## Non-goals

We also have some things we specifically want to avoid as we feel they inhibit
our ability to accomplish our goals:

* macOS or Windows support
  * These operating systems are very uncommon in the hobbyist server space, and
    we feel our effort is better spent elsewhere.
* Docker support
  * Docker tends to generate a high volume of support requests that are solely
    due to Docker itself or how users are using Docker. In attempt to mitigate
    this, we will not provide first-party Docker images. Instead, we'd recommend
    avoiding Docker and either using our pre-built statically-linked binaries
    or building from source. However, if your deployment mechanism *requires*
    Docker, it should be straightforward to build your own Docker image.
* Configuration via environment variables
  * Environment variables restrict the options for structuring configuration and
    support for them would increase the maintenance burden. If your deployment
    mechanism requires this, consider using an external tool like
    [`envsubst`][envsubst].
* Configuration compatibility with Conduit
  * To provide a secure and ergonomic configuration experience, breaking changes
    are required. However, we do intend to provide a migration tool to ease
    migration; this feature is tracked [here][migration-tool].
* Perfect database compatibility with Conduit
  * The current database compatibility status can be tracked [here][db-compat].
    In the long run, it's inevitable that changes will be made to Conduit that
    we won't want to pull in, or that we need to make changes that Conduit won't
    want to pull in.

[envsubst]: https://github.com/a8m/envsubst
[migration-tool]: https://gitlab.computer.surgery/matrix/grapevine-fork/-/issues/38
[db-compat]: https://gitlab.computer.surgery/matrix/grapevine-fork/-/issues/17

## Project management

The project's current maintainers[^1] are:

| Matrix username | GitLab username |
|-|-|
| `@charles:computer.surgery` | `charles` |
| `@benjamin:computer.surgery` | `benjamin` |
| `@xiretza:xiretza.xyz` | `Lambda` |

We would like to expand this list in the future as social trust is built and
technical competence is demonstrated by other contributors.

We require at least 1 approving code review from a maintainer[^2] before changes
can be merged. This number may increase in the future as the list of maintainers
grows.

## Expectations management

This project is run and maintained entirely by volunteers who are doing their
best. Additionally, due to our goals, the development of new features may be
slower than alternatives. We find this to be an acceptable tradeoff considering
the importance of the reliability of a project like this.

---

[^1]: A "maintainer" is someone who has the ability to close issues opened by
      someone else and merge changes.
[^2]: A maintainer approving their own change doesn't count.
