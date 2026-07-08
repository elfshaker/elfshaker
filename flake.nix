{
  inputs = {
    naersk.url = "github:nix-community/naersk";
    naersk.inputs.nixpkgs.follows = "nixpkgs";
    naersk.inputs.fenix.follows = "fenix";
    # Note: the flake.lock revision of this package determines the rust
    # version used.
    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
  };

  outputs = { self, nixpkgs, naersk, fenix }: let
    inherit (nixpkgs) lib;
    forAllSystems = lib.genAttrs lib.systems.flakeExposed;
  in {

    devShell = forAllSystems (system: let
      pkgs = import nixpkgs { inherit system; };
    in pkgs.mkShell (self.packages.${system}.elfshakerCargoConfig // {
      nativeBuildInputs = [
        pkgs.cargo-edit
        self.packages.${system}.rustToolchain
        pkgs.pkgsCross.aarch64-multiplatform-musl.stdenv.cc
        pkgs.pkgsCross.musl64.stdenv.cc
      # ] ++ lib.optionals pkgs.stdenv.hostPlatform.isx86 [ # Don't do windows cross-arch cross-compile for now.
        pkgs.pkgsCross.mingwW64.stdenv.cc
      ];
      CARGO_BUILD_TARGET = pkgs.stdenv.hostPlatform.config;
      NIX_PATH = "nixpkgs=${nixpkgs.outPath}";
    }));

    packages = forAllSystems (system: let
      pkgs = import nixpkgs { inherit system; };

      fenixPackages = fenix.packages.${system};
      rustBuildComponents = with fenixPackages; [
        stable.cargo
        stable.rustc
        targets.aarch64-unknown-linux-gnu.stable.rust-std
        targets.aarch64-unknown-linux-musl.stable.rust-std
        targets.x86_64-unknown-linux-musl.stable.rust-std
        targets.x86_64-pc-windows-gnu.stable.rust-std
      ];
      rustBuildToolchain = fenixPackages.combine rustBuildComponents;
      rustToolchain = fenixPackages.combine (rustBuildComponents ++ (with fenixPackages; [
        stable.clippy
        rust-analyzer
        stable.rust-src
        stable.rustfmt
      ]));

      naersk' = naersk.lib.${system}.override {
        cargo = rustBuildToolchain;
        rustc = rustBuildToolchain;
      };

      naerskBuildPackage = isWindows: target: args:
        naersk'.buildPackage (args // { CARGO_BUILD_TARGET = target; } // (cargoConfig isWindows));

      # On Linux, configure cross compilers.
      cargoConfig = isWindows: (lib.optionalAttrs pkgs.stdenv.isLinux {
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

      } // lib.optionalAttrs pkgs.stdenv.isDarwin {
        CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS = "-C link-arg=-Wl,-dead_strip_dylibs";
        CARGO_TARGET_X86_64_APPLE_DARWIN_RUSTFLAGS = "-C link-arg=-Wl,-dead_strip_dylibs";
      } // lib.optionalAttrs isWindows {
        CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = "x86_64-w64-mingw32-gcc";
        CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER = pkgs.writeShellScript "wine-wrapper" ''
          export WINEPREFIX="/tmp/elfshaker_testing" WINEDEBUG=-all
          export HOME=$WINEPREFIX
          export FONTCONFIG_PATH=${pkgs.buildPackages.fontconfig.out}/etc/fonts/
          mkdir -p $WINEPREFIX
          exec ${pkgs.buildPackages.wine64}/bin/wine "$@"
        '';
      });

      packages = self.packages.${system};
      args = { inherit naerskBuildPackage rustToolchain; };
      nativePlatform = pkgs.stdenv.buildPlatform;
      nativeArch = nativePlatform.qemuArch; # (the correct spelling)
      cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
      releaseVersion = "v${cargoToml.package.version}";

      releaseBundle = ''
        make_archive() {
          archive="$1"
          binary="$2"
          installed_name="$3"
          root="root-$(basename "$archive" .tar.gz)"

          mkdir -p "$root/elfshaker"
          cp "$binary" "$root/elfshaker/$installed_name"
          cp ${./README.md} "$root/elfshaker/README.md"
          cp ${./LICENSE} "$root/elfshaker/LICENSE"
          cp ${./CONTRIBUTORS} "$root/elfshaker/CONTRIBUTORS"
          tar czf "$archive" --directory "$root" elfshaker
          sha256sum "$archive" > "$archive.sha256sum"
        }
      '';


    in {
      inherit rustToolchain;
      elfshakerCargoConfig = cargoConfig false; #pkgs.stdenv.hostPlatform.isWindows;

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

      release = pkgs.runCommandNoCC "elfshaker-release" {
        nativeBuildInputs = lib.optionals nativePlatform.isDarwin [ pkgs.darwin.cctools ];
      } (
        if nativePlatform.isLinux then ''
            ${releaseBundle}

            make_archive \
              elfshaker_${releaseVersion}_aarch64-unknown-linux-musl.tar.gz \
              ${packages.elfshaker-aarch64-musl}/bin/elfshaker \
              elfshaker

            make_archive \
              elfshaker_${releaseVersion}_x86_64-unknown-linux-musl.tar.gz \
              ${packages.elfshaker-x86_64-musl}/bin/elfshaker \
              elfshaker

            ${lib.optionalString (nativePlatform.isx86) ''
              make_archive \
                elfshaker_${releaseVersion}_x86_64-pc-windows-gnu.tar.gz \
                ${packages.elfshaker-x86_64-windows}/bin/elfshaker.exe \
                elfshaker.exe
            ''}

            mkdir $out
            cp *.tar.gz* $out
        ''
        else if nativePlatform.isDarwin then ''
          ${releaseBundle}

          otool -L ${packages.elfshaker}/bin/elfshaker > linked-libraries
          sed '1d' linked-libraries > linked-dependencies
          if grep -F '${builtins.storeDir}/' linked-dependencies; then
            echo "Darwin release binary contains Nix runtime dependencies" >&2
            exit 1
          fi

          make_archive \
            elfshaker_${releaseVersion}_${nativePlatform.config}.tar.gz \
            ${packages.elfshaker}/bin/elfshaker \
            elfshaker

          mkdir $out
          cp *.tar.gz* $out
        ''
        else builtins.throw "elfshaker flake: Unknown platform ${nativePlatform.config}"
      );
    });
  };
}
