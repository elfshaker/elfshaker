{
  inputs = {
    naersk.url = "github:nix-community/naersk/cddffb5aa211f50c4b8750adbec0bbbdfb26bb9f";
    naersk.inputs.nixpkgs.follows = "nixpkgs";
    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";
    nixpkgs.url = "github:NixOS/nixpkgs/release-22.05";
    utils.url = "github:numtide/flake-utils/7e2a3b3dfd9af950a856d66b0a7d01e3c18aa249";
  };

  outputs = { self, nixpkgs, utils, naersk, fenix }:
    utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        naersk-lib = pkgs.callPackage naersk { };
      in
      {
        defaultPackage = naersk-lib.buildPackage {
          root = ./.;
        };

        defaultApp = utils.lib.mkApp {
          drv = self.defaultPackage."${system}";
        };

        devShell = with pkgs; mkShell {
          nativeBuildInputs = [
            pre-commit
            pkgs.pkgsStatic.buildPackages.gcc # aarch64-unknown-linux-musl-gcc
            pkg-config
            gcc
            (python310.withPackages (pkgs: with pkgs; [
              msgpack
              ipython
            ]))
          ] ++ (with fenix.packages.${system}; [
            (
              combine [
                stable.cargo
                stable.clippy
                rust-analyzer
                stable.rust-src
                stable.rustc
                stable.rustfmt
                targets.aarch64-unknown-linux-gnu.stable.rust-std
                targets.aarch64-unknown-linux-musl.stable.rust-std
                targets.x86_64-unknown-linux-musl.stable.rust-std
              ]
            )
          ]);

          CC_aarch64_unknown_linux_musl = "aarch64-unknown-linux-musl-gcc";
          CC_aarch64_unknown_linux_gnu = "cc";

          CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS = "-L${pkgs.pkgsStatic.buildPackages.gcc.cc.lib}/aarch64-unknown-linux-musl/lib -lstatic=stdc++";
          CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER = "aarch64-unknown-linux-musl-ld";
          CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER = "cc";

          NIX_PATH = "nixpkgs=${nixpkgs.outPath}";
        };
      });
}

