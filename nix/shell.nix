# Keep sorted
{ default
, engage
, complement
, go
, inputs
, jq
, lib
, lychee
, markdownlint-cli
, mdbook
, mkShell
, system
, toolchain
}:

mkShell {
  env = default.env // {
    # Rust Analyzer needs to be able to find the path to default crate
    # sources, and it can read this environment variable to do so. The
    # `rust-src` component is required in order for this to work.
    RUST_SRC_PATH = "${toolchain}/lib/rustlib/src/rust/library";

    # See the doc comment on `use_xtask_path` in `xtask/src/main.rs`.
    GRAPEVINE_XTASK_PATH = lib.makeBinPath [
      # Keep sorted
      complement
      go
    ];
  };

  # Development tools
  nativeBuildInputs = [
    # Always use nightly rustfmt because most of its options are unstable
    #
    # This needs to come before `toolchain` in this list, otherwise
    # `$PATH` will have stable rustfmt instead.
    inputs.fenix.packages.${system}.latest.rustfmt

    # Keep sorted
    engage
    jq
    lychee
    markdownlint-cli
    mdbook
    toolchain
  ]
  ++
  default.nativeBuildInputs
  ++
  default.propagatedBuildInputs
  ++
  default.buildInputs;
}
