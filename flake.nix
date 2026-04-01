{
  description = "A Linux shell written in Rust";

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
      pname = "shed";
      version = "0.8.3";

      src = self;

      cargoLock = {
        lockFile = ./Cargo.lock;
      };

      passthru.shellPath = "/bin/shed";

      checkPhase = ''
        cargo test -- --test-threads=1
      '';

      meta = with pkgs.lib; {
        description = "A Linux shell written in Rust";
        homepage = "https://github.com/km-clay/shed";
        license = licenses.mit;
        maintainers = [ ];
        platforms = platforms.linux;
      };
    };
  }) // {
    nixosModules.shed = import ./nix/module.nix;
    homeModules.shed = import ./nix/hm-module.nix;

    overlays.default = final: prev: {
      shed = self.packages.${final.stdenv.hostPlatform.system}.default;
    };
  };
}
