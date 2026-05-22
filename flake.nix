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

    # Rust toolchain with the musl target std lib available, so
    # `cargo build --target x86_64-unknown-linux-musl` works inside
    # `nix develop` without needing rustup.
    rustToolchain = pkgs.rust-bin.stable.latest.default.override {
      targets = [ "x86_64-unknown-linux-musl" ];
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

    packages.default = pkgs.rustPlatform.buildRustPackage {
      pname = "shed";
      version = "0.19.3";

      src = self;

      cargoLock = {
        lockFile = ./Cargo.lock;
      };

      passthru.shellPath = "/bin/shed";

      SHED_DOC_DIR = "${placeholder "out"}/share/shed/doc";

      postInstall = ''
        mkdir -p $out/share/shed/doc
        cp doc/*.txt $out/share/shed/doc
      '';

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
