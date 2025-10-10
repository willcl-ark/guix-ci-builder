{
  description = "bitcoin core pull request guix builder";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/25.05";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs = {
        nixpkgs.follows = "nixpkgs";
      };
    };
  };

  outputs =
    {
      nixpkgs,
      flake-utils,
      rust-overlay,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
      in
      with pkgs;
      let
        package =
          let
            rustPlatform = makeRustPlatform {
              cargo = rust-bin.stable.latest.default;
              rustc = rust-bin.stable.latest.default;
            };
          in
          rustPlatform.buildRustPackage rec {
            pname = "core-guix-builder";
            version = "0.1.0";

            src = ./.;

            cargoLock = {
              lockFile = ./Cargo.lock;
            };

            nativeBuildInputs = [
              pkg-config
            ];

            buildInputs = [
              openssl
            ];

            meta = with lib; {
              description = "Bitcoin Core PR guix builder";
              license = licenses.mit;
              maintainers = [ ];
            };
          };
      in
      {
        formatter = nixfmt-tree;

        packages.default = package;

        apps.default = {
          type = "app";
          program = "${package}/bin/core-guix-builder";
        };

        devShells.default = mkShell {
          nativeBuildInputs = [ pkg-config ];
          buildInputs = [
            rust-bin.stable.latest.default
            openssl
          ];
        };
      }
    );
}
