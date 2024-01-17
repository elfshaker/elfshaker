{
  inputs = {
    naersk.url = "github:nix-community/naersk";
    naersk.inputs.nixpkgs.follows = "nixpkgs";
    # Note: the flake.lock revision of this package determines the rust
    # version used.
    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-23.05";
  };

  outputs = { self, nixpkgs, naersk, fenix }: let
    inherit (nixpkgs) lib;
    forAllSystems = lib.genAttrs lib.systems.flakeExposed;
  in {

    devShell = forAllSystems (system: let
      pkgs = import nixpkgs { inherit system; };
    in pkgs.mkShell (self.packages.${system}.elfshakerCargoConfig // {
      nativeBuildInputs = [
        self.packages.${system}.rustToolchain
        pkgs.pkgsCross.aarch64-multiplatform-musl.stdenv.cc
        pkgs.pkgsCross.musl64.stdenv.cc
        pkgs.pkgsCross.mingwW64.stdenv.cc
      ];
      CARGO_BUILD_TARGET = pkgs.stdenv.hostPlatform.config;
      NIX_PATH = "nixpkgs=${nixpkgs.outPath}";
    }));

    packages = forAllSystems (system: let
      pkgs = import nixpkgs { inherit system; };

      rustToolchain = with fenix.packages.${system};
        combine [
          stable.cargo
          stable.clippy
          rust-analyzer
          stable.rust-src
          stable.rustc
          stable.rustfmt
          targets.aarch64-unknown-linux-gnu.stable.rust-std
          targets.aarch64-unknown-linux-musl.stable.rust-std
          targets.aarch64-unknown-linux-musl.stable.rust-std
          targets.x86_64-unknown-linux-musl.stable.rust-std
          targets.x86_64-pc-windows-gnu.stable.rust-std
        ];

      naersk' = naersk.lib.${system}.override {
        cargo = rustToolchain;
        rustc = rustToolchain;
      };

      naerskBuildPackage = target: args:
        naersk'.buildPackage (args // { CARGO_BUILD_TARGET = target; } // cargoConfig);

      # On Linux, configure cross compilers.
      cargoConfig = lib.optionalAttrs pkgs.stdenv.isLinux {
        CC_aarch64_unknown_linux_musl = "aarch64-unknown-linux-musl-gcc";
        CC_aarch64_unknown_linux_gnu = "cc";
        CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER = "aarch64-unknown-linux-musl-ld";
        CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER = "cc";

        CC_x86_64_unknown_linux_musl = "x86_64-unknown-linux-musl-gcc";
        CC_x86_64_unknown_linux_gnu = "cc";
        # CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS = [
        #   "-L${pkgs.pkgsCross.musl64.pkgsStatic.stdenv.cc.cc.lib}/x86_64-unknown-linux-musl/lib"
        #   "-lstatic=stdc++"
        #   "-Ctarget-feature=+crt-static"
        #   # The default of static pie executables results in the error message:
        #   # > x86_64-unknown-linux-musl-ld: gcc-12.2.0-lib/x86_64-unknown-linux-musl/lib/libstdc++.a(compatibility.o):
        #   # >   relocation R_X86_64_32 against symbol `__gxx_personality_v0' can not be used when making a PIE object; recompile with -fPIE
        #   # > x86_64-unknown-linux-musl-ld: failed to set dynamic section sizes: bad value
        #   # ... But only in the test binary, presumably because it somehow ends
        #   # up using the symbol in a problematic way.
        #   "-Crelocation-model=pic"
        # ];
        CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER = "x86_64-unknown-linux-musl-ld";
        CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER = "cc";

        # (Not fully tested, build gets as far as programming errors
        # relating to our handling of file permissions which needs
        # fixing, but this may work.)
        CC_x86_64_pc_windows_gnu = "x86_64-w64-mingw32-gcc";
        CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS = "-L${pkgs.pkgsCross.mingwW64.windows.mingw_w64_pthreads}/lib";

        CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = "x86_64-w64-mingw32-gcc";
        CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER = pkgs.writeShellScript "wine-wrapper" ''
          export WINEPREFIX="/tmp/elfshaker_testing" WINEDEBUG=-all
          export HOME=$WINEPREFIX
          export FONTCONFIG_PATH=${pkgs.buildPackages.fontconfig.out}/etc/fonts/
          mkdir -p $WINEPREFIX
          exec ${pkgs.buildPackages.wine64}/bin/wine64 "$@"
        '';
      };

      packages = self.packages.${system};
      args = { inherit naerskBuildPackage rustToolchain; };
      nativePlatform = pkgs.stdenv.buildPlatform;
      nativeArch = nativePlatform.qemuArch; # (the correct spelling)


    in {
      inherit rustToolchain;
      elfshakerCargoConfig = cargoConfig;

      # Native package build.
      default = packages.elfshaker;
      elfshaker = pkgs.callPackage ./elfshaker.nix args;

      # Release binaries.
      elfshaker-aarch64-musl = pkgs.pkgsCross.aarch64-multiplatform-musl.callPackage ./elfshaker.nix args;
      elfshaker-x86_64-musl = pkgs.pkgsCross.musl64.callPackage ./elfshaker.nix args;
      elfshaker-x86_64-windows = pkgs.pkgsCross.mingwW64.callPackage ./elfshaker.nix args;
      # Note: aarch64-windows cross compiler doesn't yet exist.
      # elfshaker-aarch64-windows = pkgs.pkgsCross.?.callPackage ./elfshaker.nix args;

      # Note: Can't cross compile to darwin from linux, can't currently
      # cross compile between architectures on darwin (but these work on
      # their respective architectures).
      elfshaker-aarch64-darwin = pkgs.pkgsCross.aarch64-darwin.callPackage ./elfshaker.nix args;
      elfshaker-x86_64-darwin = pkgs.pkgsCross.x86_64-darwin.callPackage ./elfshaker.nix args;

      release = pkgs.runCommandNoCC "elfshaker-release" {} (
        if nativePlatform.isLinux then ''
            tar czf elfshaker-aarch64-musl.tar.gz --directory ${packages.elfshaker-aarch64-musl}/bin elfshaker
            sha256sum elfshaker-aarch64-musl.tar.gz > elfshaker-aarch64-musl.tar.gz.sha256sum

            tar czf elfshaker-x86_64-musl.tar.gz --directory ${packages.elfshaker-x86_64-musl}/bin elfshaker
            sha256sum elfshaker-x86_64-musl.tar.gz > elfshaker-x86_64-musl.tar.gz.sha256sum

            # TODO: Windows cross compile.

            mkdir $out
            cp *.tar.gz* $out
        ''
        else if nativePlatform.isDarwin then ''
          tar czf elfshaker-${nativePlatform.darwinArch}-darwin${nativePlatform.darwinMinVersion}.tar.gz --directory ${packages.elfshaker}/bin elfshaker
          sha256sum elfshaker-${nativePlatform.darwinArch}-darwin${nativePlatform.darwinMinVersion}.tar.gz > elfshaker-${nativePlatform.darwinArch}-darwin${nativePlatform.darwinMinVersion}.tar.gz.sha256sum

          mkdir $out
          cp *.tar.gz* $out
        ''
        else builtins.throw "elfshaker flake: Unknown platform ${nativePlatform.config}"
      );
    });
  };
}
