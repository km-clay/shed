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
      overlays = [
        (_: prev: {
          # crate vendoring only works on newer versions if we change
          # the user agent for the curl call, for some reason.
          fetchurl = prev.fetchurl // {
            __functor = _: arg: (prev.fetchurl arg).overrideAttrs (old: {
              curlOptsList = (old.curlOptsList or [ ]) ++ [ "--user-agent" "nixpkgs-fetchurl" ];
            });
          };
        })
        rust-overlay.overlays.default
      ];
    };

    rustToolchain = pkgs.rust-bin.stable.latest.default.override {
      targets = [
        "x86_64-unknown-linux-musl"
        "aarch64-unknown-linux-gnu"
        "aarch64-linux-android"
        "x86_64-apple-darwin"
        "aarch64-apple-darwin"
      ];
    };

    rustPlatform = pkgs.makeRustPlatform {
      cargo = rustToolchain;
      rustc = rustToolchain;
    };

    checkTargets = [
      "x86_64-unknown-linux-gnu"
      "aarch64-unknown-linux-gnu"
      "x86_64-unknown-linux-musl"
      "x86_64-apple-darwin"
      "aarch64-apple-darwin"
    ];

    # android can't go through cargo-zigbuild
    # so we need a dedicated checker for it
    androidTriple = "aarch64-linux-android";
    androidSupported = system == "x86_64-linux";
    androidPkgs = import nixpkgs {
      inherit system;
      overlays = [ rust-overlay.overlays.default ];
      config.allowUnfree = true;
    };
    androidCC = androidPkgs.pkgsCross.aarch64-android-prebuilt.stdenv.cc;

    androidCheck = pkgs.stdenv.mkDerivation {
      name = "check-${androidTriple}";
      src = self;
      cargoDeps = rustPlatform.importCargoLock { lockFile = ./Cargo.lock; };
      nativeBuildInputs = [
        rustToolchain
        rustPlatform.cargoSetupHook
        androidCC
      ];
      buildPhase = ''
        runHook preBuild
        export HOME=$(mktemp -d)
        export CC_aarch64_linux_android="${androidCC}/bin/${androidCC.targetPrefix}cc"
        export AR_aarch64_linux_android="${androidCC}/bin/${androidCC.targetPrefix}ar"
        export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="${androidCC}/bin/${androidCC.targetPrefix}cc"
        cargo check --target ${androidTriple} --offline
        runHook postBuild
      '';
      installPhase = "touch $out";
      dontFixup = true;
    };

    mkCheck = triple:
      pkgs.stdenv.mkDerivation {
        name = "check-${triple}";
        src = self;
        cargoDeps = rustPlatform.importCargoLock { lockFile = ./Cargo.lock; };
        nativeBuildInputs = [
          rustToolchain
          rustPlatform.cargoSetupHook
          pkgs.zig
          pkgs.cargo-zigbuild
        ];
        buildPhase = ''
          runHook preBuild
          export HOME=$(mktemp -d)
          export ZIG_GLOBAL_CACHE_DIR=$(mktemp -d)
          cargo-zigbuild check --target ${triple} --offline
          runHook postBuild
        '';
        installPhase = "touch $out";
        dontFixup = true;
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

      CC_x86_64_unknown_linux_musl =
        "${pkgs.pkgsCross.musl64.stdenv.cc}/bin/${pkgs.pkgsCross.musl64.stdenv.cc.targetPrefix}cc";
      AR_x86_64_unknown_linux_musl =
        "${pkgs.pkgsCross.musl64.stdenv.cc}/bin/${pkgs.pkgsCross.musl64.stdenv.cc.targetPrefix}ar";
    };

    packages.default = rustPlatform.buildRustPackage {
      pname = "shed";
      version = "0.42.11";

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

    checks = (builtins.listToAttrs (map (t: {
      name = "check-${t}";
      value = mkCheck t;
    }) checkTargets))
    // pkgs.lib.optionalAttrs androidSupported {
      # only fold this in if our system can build it
      "check-${androidTriple}" = androidCheck;
    }
    // {
      tests = self.packages.${system}.default;
    };
  }) // {
    nixosModules.shed = import ./nix/module.nix;
    homeModules.shed = import ./nix/hm-module.nix;

    overlays.default = final: prev: {
      shed = self.packages.${final.stdenv.hostPlatform.system}.default;
    };
  };
}
