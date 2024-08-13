# Dependencies (keep sorted)
{ craneLib
, inputs
, jq
, lib
, pkgsBuildHost
, rocksdb
, rust
, rust-jemalloc-sys
, stdenv

# Options (keep sorted)
, default-features ? true
, all-features ? false
, features ? []
, profile ? "release"
}:

let
  # We perform default-feature unification in nix, because some of the dependencies
  # on the nix side depend on feature values.
  cargoManifest = lib.importTOML "${inputs.self}/Cargo.toml";
  allDefaultFeatures = cargoManifest.features.default;
  allFeatures = lib.unique (
    lib.remove "default" (lib.attrNames cargoManifest.features) ++
    lib.attrNames
      (lib.filterAttrs (_: dependency: dependency.optional or false)
        cargoManifest.dependencies));
  features' = lib.unique
    (features ++
      lib.optionals default-features allDefaultFeatures ++
      lib.optionals all-features allFeatures);

  featureEnabled = feature : builtins.elem feature features';

  # This derivation will set the JEMALLOC_OVERRIDE variable, causing the
  # tikv-jemalloc-sys crate to use the nixpkgs jemalloc instead of building it's
  # own. In order for this to work, we need to set flags on the build that match
  # whatever flags tikv-jemalloc-sys was going to use. These are dependent on
  # which features we enable in tikv-jemalloc-sys.
  rust-jemalloc-sys' = (rust-jemalloc-sys.override {
    # tikv-jemalloc-sys/unprefixed_malloc_on_supported_platforms feature
    unprefixed = true;
  });

  buildDepsOnlyEnv =
    let
      rocksdb' = rocksdb.override {
        jemalloc = rust-jemalloc-sys';
        enableJemalloc = featureEnabled "jemalloc";
      };
    in
    {
      NIX_OUTPATH_USED_AS_RANDOM_SEED = "randomseed";
      CARGO_PROFILE = profile;
      ROCKSDB_INCLUDE_DIR = "${rocksdb'}/include";
      ROCKSDB_LIB_DIR = "${rocksdb'}/lib";
    }
    //
    (import ./cross-compilation-env.nix {
      # Keep sorted
      inherit
        lib
        pkgsBuildHost
        rust
        stdenv;
    });

  buildPackageEnv = {
    GRAPEVINE_VERSION_EXTRA = inputs.self.shortRev or inputs.self.dirtyShortRev;
  } // buildDepsOnlyEnv;

  commonAttrs = {
    # Reading from cargoManifest directly instead of using
    # createNameFromCargoToml to avoid IFD
    pname = cargoManifest.package.name;
    version = cargoManifest.package.version;

    src = let filter = inputs.nix-filter.lib; in filter {
      root = inputs.self;

      # Keep sorted
      include = [
        "Cargo.lock"
        "Cargo.toml"
        "src"
      ];
    };

    dontStrip = profile == "dev";

    buildInputs = lib.optional (featureEnabled "jemalloc") rust-jemalloc-sys';

    nativeBuildInputs = [
      # bindgen needs the build platform's libclang. Apparently due to "splicing
      # weirdness", pkgs.rustPlatform.bindgenHook on its own doesn't quite do the
      # right thing here.
      pkgsBuildHost.rustPlatform.bindgenHook

      # We don't actually depend on `jq`, but crane's `buildPackage` does, but
      # its `buildDepsOnly` doesn't. This causes those two derivations to have
      # differing values for `NIX_CFLAGS_COMPILE`, which contributes to spurious
      # rebuilds of bindgen and its depedents.
      jq
    ];
  };
in

craneLib.buildPackage (commonAttrs // {
  cargoArtifacts = craneLib.buildDepsOnly (commonAttrs // {
    env = buildDepsOnlyEnv;
  });

  cargoExtraArgs = "--locked --no-default-features "
    + lib.optionalString
      (features' != [])
      "--features " + (builtins.concatStringsSep "," features');

  # This is redundant with CI
  doCheck = false;

  env = buildPackageEnv;

  passthru = {
    env = buildPackageEnv;
  };

  meta.mainProgram = commonAttrs.pname;
})
