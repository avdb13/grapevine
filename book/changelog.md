# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog][keep-a-changelog], and this project
adheres to [Semantic Versioning][semver].

[keep-a-changelog]: https://keepachangelog.com/en/1.0.0/
[semver]: https://semver.org/spec/v2.0.0.html

<!--

Changelog sections must appear in the following order if they appear for a
particular version so that attention can be drawn to the important parts:

1. Security
2. Removed
3. Deprecated
4. Changed
5. Fixed
6. Added

Entries within each section should be sorted by merge order. If multiple changes
result in a single entry, choose the merge order of the first or last change.

-->

## Unreleased

<!-- TODO: Change "will be" to "is" on release -->

This will be the first release of Grapevine since it was forked from Conduit
0.7.0.

### Security

1. Prevent XSS via user-uploaded media.
   ([!8](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/8))
2. Switch from incorrect, hand-rolled `X-Matrix` `Authorization` parser to the
   much better implementation provided by Ruma.
   ([!31](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/31))
   * This is not practically exploitable to our knowledge, but this change does
     reduce risk.
3. Switch to a more trustworthy password hashing library.
   ([!29](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/29))
   * This is not practically exploitable to our knowledge, but this change does
     reduce risk.
4. Don't return redacted events from the search endpoint.
   ([!41 (f74043d)](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/41/diffs?commit_id=f74043df9aa59b406b5086c2e9fa2791a31aa41b),
   [!41 (83cdc9c)](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/41/diffs?commit_id=83cdc9c708cd7b50fe1ab40ea6a68dcf252c190b))
5. Prevent impersonation in EDUs.
   ([!41 (da99b07)](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/41/diffs?commit_id=da99b0706e683a2d347768efe5b50676abdf7b44))
   * `m.signing_key_update` was not affected by this bug.
6. Verify PDUs and transactions against the temporally-correct signing keys.
   ([!41 (9087da9)](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/41/diffs?commit_id=9087da91db8585f34d026a48ba8fdf64865ba14d))
7. Only allow the admin bot to change the room ID that the admin room alias
   points to.
   ([!42](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/42))

### Removed

1. Remove update checker.
   ([17a0b34](https://gitlab.computer.surgery/matrix/grapevine-fork/-/commit/17a0b3430934fbb8370066ee9dc3506102c5b3f6))
2. Remove optional automatic display name emoji for newly registered users.
   ([cddf699](https://gitlab.computer.surgery/matrix/grapevine-fork/-/commit/cddf6991f280008b5af5acfab6a9719bb0cfb7f1))
3. Remove admin room welcome message on first startup.
   ([c9945f6](https://gitlab.computer.surgery/matrix/grapevine-fork/-/commit/c9945f6bbac6e22af6cf955cfa99826d4b04fe8c))
4. Remove incomplete presence implementation.
   ([f27941d](https://gitlab.computer.surgery/matrix/grapevine-fork/-/commit/f27941d5108acda250921c6a58499a46568fd030))
5. Remove Debian packaging.
   ([d41f0fb](https://gitlab.computer.surgery/matrix/grapevine-fork/-/commit/d41f0fbf72dae6562358173f425d23bb0e174ca2))
6. Remove Docker packaging.
   ([!48](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/48))

### Changed

1. **BREAKING:** Rename `conduit_cache_capacity_modifier` configuration option
   to `cache_capacity_modifier`.
   ([5619d7e](https://gitlab.computer.surgery/matrix/grapevine-fork/-/commit/5619d7e3180661731800e253b558b88b407d2ae7))
   * If you are explicitly setting this configuration option, make sure to
     change its name before updating.
2. **BREAKING:** Rename Conduit to Grapevine.
   ([360e020](https://gitlab.computer.surgery/matrix/grapevine-fork/-/commit/360e020b644bd012ed438708b661a25fbd124f68))
   * The `CONDUIT_VERSION_EXTRA` build-time environment variable has been
     renamed to `GRAPEVINE_VERSION_EXTRA`. This change only affects distribution
     packagers or non-Nix users who are building from source. If you fall into
     one of those categories *and* were explicitly setting this environment
     variable, make sure to change its name before building Grapevine.
3. **BREAKING:** Change the default port from 8000 to 6167.
   ([f205280](https://gitlab.computer.surgery/matrix/grapevine-fork/-/commit/f2052805201f0685d850592b1c96f4861c58fb22))
   * If you relied on the default port being 8000, either update your other
     configuration to use the new port, or explicitly configure Grapevine's port
     to 8000.
4. Improve tracing spans and events.
   ([!11 (a275db3)](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/11/diffs?commit_id=a275db3847b8d5aaa0c651a686c19cfbf9fdb8b5)
   (merged as [5172f66](https://gitlab.computer.surgery/matrix/grapevine-fork/-/commit/5172f66c1a90e0e97b67be2897ae59fbc00208a4)),
   [!11 (a275db3)](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/11/diffs?commit_id=a275db3847b8d5aaa0c651a686c19cfbf9fdb8b5)
   (merged as [5172f66](https://gitlab.computer.surgery/matrix/grapevine-fork/-/commit/5172f66c1a90e0e97b67be2897ae59fbc00208a4)),
   [!11 (f556fce)](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/11/diffs?commit_id=f556fce73eb7beec2ed7b1781df0acdf47920d9c)
   (merged as [ac42e0b](https://gitlab.computer.surgery/matrix/grapevine-fork/-/commit/ac42e0bfff6af8677636a3dc1a56701a3255071d)),
   [!18](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/18),
   [!26](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/26),
   [!50](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/50),
   [!52](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/52),
   [!54](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/54),
   [!56](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/56))
5. Stop returning unnecessary member counts from `/_matrix/client/{r0,v3}/sync`.
   ([!12](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/12))
6. **BREAKING:** Allow federation by default.
   ([!24](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/24))
   * If you relied on federation being disabled by default, make sure to
     explicitly disable it before upgrading.
7. **BREAKING:** Remove the `[global]` section from the configuration file.
   ([!38](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/38))
   * Details on how to migrate can be found in the merge request's description.
8. **BREAKING:** Allow specifying multiple transport listeners in the
   configuration file.
   ([!39](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/39))
   * Details on how to migrate can be found in the merge request's description.
9. Increase default log level so that span information is included.
   ([!50](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/50))
10. **BREAKING:** Reorganize config into sections.
    ([!49](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/49))
    * Details on how to migrate can be found in the merge request's description.

### Fixed

1. Fix questionable numeric conversions.
   ([71c48f6](https://gitlab.computer.surgery/matrix/grapevine-fork/-/commit/71c48f66c4922813c2dc30b7b875200e06ce4b75))
2. Stop sending no-longer-valid cached responses from the
   `/_matrix/client/{r0,v3}/sync` endpoints.
   ([!7](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/7))
3. Stop returning extra E2EE device updates from `/_matrix/client/{r0,v3}/sync`
   as that violates the specification.
   ([!12](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/12))
4. Make certain membership state transitions work correctly again.
   ([!16](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/16))
   * For example, it was previously impossible to unban users from rooms.
5. Ensure that `tracing-flame` flushes all its data before the process exits.
   ([!20 (263edcc)](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/20/diffs?commit_id=263edcc8a127ad2a541a3bb6ad35a8a459ea5616))
6. Reduce the likelihood of locking up the async runtime.
   ([!19](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/19))
7. Fix dynamically linked jemalloc builds.
   ([!23](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/23))
8. Fix search results not including subsequent pages in certain situations.
   ([!35 (0cdf032)](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/35/diffs?commit_id=0cdf03288ab8fa363c313bd929c8b5183d14ab77))
9. Fix search results missing events in subsequent pages in certain situations.
   ([!35 (3551a6e)](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/35/diffs?commit_id=3551a6ef7a29219b9b30f50a7e8c92b92debcdcf))
10. Only process admin commands if the admin bot is in the admin room.
    ([!43](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/43))

### Added

1. Add various conveniences for users of the Nix package.
   ([51f9650](https://gitlab.computer.surgery/matrix/grapevine-fork/-/commit/51f9650ca7bc9378690d331192c85fea3c151b58),
   [bbb1a6f](https://gitlab.computer.surgery/matrix/grapevine-fork/-/commit/bbb1a6fea45b16e8d4f94c1afbf7fa22c9281f37))
2. Add a NixOS module.
   ([33e7a46](https://gitlab.computer.surgery/matrix/grapevine-fork/-/commit/33e7a46b5385ea9035c9d13c6775d63e5626a4c7))
3. Add a Conduit compat mode.
   ([a25f2ec](https://gitlab.computer.surgery/matrix/grapevine-fork/-/commit/a25f2ec95045c5620c98eead88197a0bf13e6bb3))
   * **BREAKING:** If you're migrating from Conduit, this option must be enabled
     or else your homeserver will refuse to start.
4. Include `GRAPEVINE_VERSION_EXTRA` information in the
   `/_matrix/federation/v1/version` endpoint.
   ([509b70b](https://gitlab.computer.surgery/matrix/grapevine-fork/-/commit/509b70bd827fec23b88e223b57e0df3b42cede34))
5. Allow multiple tracing subscribers to be active at once.
   ([!20 (7a154f74)](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/20/diffs?commit_id=7a154f74166c1309ca5752149e02bbe44cd91431))
6. Allow configuring the filter for `tracing-flame`.
   ([!20 (507de06)](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/20/diffs?commit_id=507de063f53f52e0cf8e2c1a67215a5ad87bb35a))
7. Collect HTTP response time metrics via OpenTelemetry and optionally expose
   them as Prometheus metrics. This functionality is disabled by default.
   ([!22](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/22))
8. Collect metrics for lookup results (e.g. cache hits/misses).
   ([!15](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/15),
   [!36](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/36))
9. Add configuration options for controlling the log format and colors.
   ([!46](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/46))
10. Recognize the `!admin` prefix to invoke admin commands.
    ([!45](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/45))
11. Add the `set-tracing-filter` admin command to change log/metrics/flame
    filters dynamically at runtime.
    ([!49](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/49))
12. Add more configuration options.
    ([!49](https://gitlab.computer.surgery/matrix/grapevine-fork/-/merge_requests/49))
    * `observability.traces.filter`: The `tracing` filter to use for
      OpenTelemetry traces.
    * `observability.traces.endpoint`: Where OpenTelemetry should send traces.
    * `observability.flame.filter`: The `tracing` filter for `tracing-flame`.
    * `observability.flame.filename`: Where `tracing-flame` will write its
      output.
    * `observability.logs.timestamp`: Whether timestamps should be included in
      the logs.
