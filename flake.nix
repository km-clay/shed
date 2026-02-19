{
  description = "A very basic flake";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
  flake-utils.lib.eachDefaultSystem (system:
  let
    pkgs = import nixpkgs { inherit system; };
  in
  {
    packages.default = pkgs.rustPlatform.buildRustPackage {
      pname = "fern";
      version = "0.1.0";

      src = self;

      cargoLock = {
        lockFile = ./Cargo.lock;
      };

      meta = with pkgs.lib; {
        description = "A Linux shell written in Rust";
        homepage = "https://github.com/km-clay/fern";
        license = licenses.mit;
        maintainers = [ ];
        platforms = platforms.linux;
      };
    };
  });
}
