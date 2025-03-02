{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      pkgs = import nixpkgs {
        system = "x86_64-linux"; # Replace with your target system if necessary
      };
      slashBuild = pkgs.rustPlatform.buildRustPackage rec {
        pname = "slash";
        version = "v0.2.0";

        src = pkgs.fetchFromGitHub {
          owner = "pagedMov";
          repo = "slash";
          rev = "65a7a713a954c0f3fba668c6d7e0cdd023f705f7";
          hash = "sha256-AVBDv0HQn7hAGo0tW1ZFCdfO4+3VJQ0mCDkow8skD7U=";
        };

        doCheck = false; # TODO: Find a way to make tests work

        cargoHash = "sha256-lekH6AESWpKjp6mCW7KgN6ACNcG8bHAg4Pu4OXhGJ3Y=";

        nativeBuildInputs = [
          pkgs.openssl
          pkgs.openssl.dev
          pkgs.pkg-config
        ];

        PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig";
        passthru = {
          shellPath = "/bin/slash";
        };
      };
    in
    {
      packages.${pkgs.system}.default = slashBuild;

      devShells.default = pkgs.mkShell {
        nativeBuildInputs = [
          pkgs.rust-bin.stable.latest.default
          pkgs.gcc
          pkgs.clang
          pkgs.pkg-config
          pkgs.libgit2
          pkgs.libssh2
          pkgs.libssh2.dev
          pkgs.openssl
          pkgs.openssl.dev
          pkgs.llvm
          pkgs.libclang
          pkgs.pam
        ];

        shellHook = ''
          exec slash
        '';
      };
    };
}
