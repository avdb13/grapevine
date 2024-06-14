{
  # Keep sorted
  inputs = {
    attic.url = "github:zhaofengli/attic?ref=main";
    crane = { url = "github:ipetkov/crane?ref=master"; inputs.nixpkgs.follows = "nixpkgs"; };
    fenix = { url = "github:nix-community/fenix?ref=main"; inputs.nixpkgs.follows = "nixpkgs"; };
    flake-compat = { url = "github:edolstra/flake-compat?ref=master"; flake = false; };
    flake-utils.url = "github:numtide/flake-utils?ref=main";
    nix-filter.url = "github:numtide/nix-filter?ref=main";
    nixpkgs.url = "github:NixOS/nixpkgs?ref=nixos-unstable";
  };

  outputs = inputs:
    let
      # Keep sorted
      mkScope = pkgs: pkgs.lib.makeScope pkgs.newScope (self: {
        craneLib =
          (inputs.crane.mkLib pkgs).overrideToolchain self.toolchain;

        default = self.callPackage ./nix/pkgs/default {};

        inherit inputs;

        # Return a new scope with overrides applied to the 'default' package
        overrideDefaultPackage = args: self.overrideScope (final: prev: {
          default = prev.default.override args;
        });

        shell = self.callPackage ./nix/shell.nix {};

        # The Rust toolchain to use
        toolchain = inputs
          .fenix
          .packages
          .${pkgs.pkgsBuildHost.system}
          .fromToolchainFile {
            file = ./rust-toolchain.toml;

            # See also `rust-toolchain.toml`
            sha256 = "sha256-opUgs6ckUQCyDxcB9Wy51pqhd0MPGHUVbwRKKPGiwZU=";
          };
      });
    in
    inputs.flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = inputs.nixpkgs.legacyPackages.${system};
      in
      {
        packages = {
          default = (mkScope pkgs).default;
        }
        //
        builtins.listToAttrs
          (builtins.concatLists
            (builtins.map
              (crossSystem:
                let
                  binaryName = "static-${crossSystem}";
                  pkgsCrossStatic =
                    (import inputs.nixpkgs {
                      inherit system;
                      crossSystem = {
                        config = crossSystem;
                      };
                    }).pkgsStatic;
                in
                [
                  # An output for a statically-linked binary
                  {
                    name = binaryName;
                    value = (mkScope pkgsCrossStatic).default;
                  }
                ]
              )
              [
                "x86_64-unknown-linux-musl"
                "aarch64-unknown-linux-musl"
              ]
            )
          );

        devShells.default = (mkScope pkgs).shell;
        devShells.all-features = ((mkScope pkgs).overrideDefaultPackage {
          all-features = true;
        }).shell;
      }
    )
    //
    {
      nixosModules.default = import ./nix/modules/default inputs;
    };
}
