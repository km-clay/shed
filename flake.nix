{
  description = "A Linux shell written in Rust";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
  flake-utils.lib.eachDefaultSystem (system:
  let
    pkgs = import nixpkgs {
      inherit system;
      overlays = [ rust-overlay.overlays.default ];
    };

    rustToolchain = pkgs.rust-bin.stable.latest.default.override {
      targets = [
        "x86_64-unknown-linux-musl"
        "x86_64-apple-darwin"
      ];
    };

    # buildRustPackage defaults to nixpkgs's rustc, which lags behind. Build
    # an explicit rustPlatform from the same overlay toolchain the devShell
    # uses so `nix build` sees the same language features as `cargo build`.
    rustPlatform = pkgs.makeRustPlatform {
      cargo = rustToolchain;
      rustc = rustToolchain;
    };
  in
  {
    devShells.default = pkgs.mkShell {
      buildInputs = [
        rustToolchain
        pkgs.pkgsCross.musl64.stdenv.cc  # musl linker for the cross build
      ];

      # Tell cargo which linker to use for the musl target.
      CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER =
        "${pkgs.pkgsCross.musl64.stdenv.cc}/bin/${pkgs.pkgsCross.musl64.stdenv.cc.targetPrefix}cc";

      # Tell the `cc` crate (used by libsqlite3-sys build script and any
      # other -sys crate compiling C code) to use the musl cross-cc when
      # building for the musl target. Without this it falls back to host
      # gcc, which produces glibc-flavored object files that don't link
      # against musl ("undefined reference to open64 / __memcpy_chk").
      CC_x86_64_unknown_linux_musl =
        "${pkgs.pkgsCross.musl64.stdenv.cc}/bin/${pkgs.pkgsCross.musl64.stdenv.cc.targetPrefix}cc";
      AR_x86_64_unknown_linux_musl =
        "${pkgs.pkgsCross.musl64.stdenv.cc}/bin/${pkgs.pkgsCross.musl64.stdenv.cc.targetPrefix}ar";
    };

    packages.default = rustPlatform.buildRustPackage {
      pname = "shed";
      version = "0.40.1";

      src = self;
      cargoLock = { lockFile = ./Cargo.lock; };

      SHED_HELP_DIR = "${placeholder "out"}/share/shed/help";

      postInstall = ''
        install -Dm644 include/help/* -t $out/share/shed/help/
        install -Dm644 LICENSE -t $out/share/shed/
      '';

      passthru.shellPath = "/bin/shed";

      checkPhase = ''
        cargo test -- --test-threads=1
      '';

      meta = with pkgs.lib; {
        description = "A Linux shell written in Rust";
        homepage = "https://github.com/km-clay/shed";
        license = licenses.mit;
        maintainers = [ ];
        platforms = platforms.linux ++ platforms.darwin;
        mainProgram = "shed";
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
