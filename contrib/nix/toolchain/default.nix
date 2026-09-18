{ nixpkgs, gnu231, system }:
assert system == "aarch64-linux";
let
  pkgs = import nixpkgs { inherit system; config = {}; overlays = []; };
  static = pkgs.pkgsLLVM.pkgsStatic;
  llvm = static.llvmPackages_23;
  # pkgsLLVM.pkgsStatic itself selects GCC at this pin. Select the LLVM
  # stdenv explicitly for the subject and shipped tools.
  stdenv = pkgs.overrideCC llvm.stdenv (llvm.stdenv.cc.overrideAttrs (old: {
    # This static GCC runtime combines the unwinder into libgcc.a, while
    # Clang still requests -lgcc_eh. Supply the compatible archive name.
    postFixup = (old.postFixup or "") + ''
      runtime=$("$out/bin/${llvm.stdenv.cc.targetPrefix}clang" --print-libgcc-file-name)
      test -f "$runtime"
      mkdir -p "$out/lib"
      ln -s "$runtime" "$out/lib/libgcc_eh.a"
      echo " -L$out/lib" >> "$out/nix-support/cc-ldflags"
    '';
  }));
  gnu = import gnu231 { inherit system; config = {}; overlays = []; };
  inherit (pkgs) lib;
  isaFlags = "-march=armv8-a";
  allocator = (static.mimalloc.override { inherit stdenv; }).overrideAttrs (old: {
    env = (old.env or {}) // { NIX_CFLAGS_COMPILE = isaFlags; };
    cmakeFlags = old.cmakeFlags ++ [ "-DMI_OVERRIDE=ON" ];
  });
  # An object before the libraries gives malloc ownership to mimalloc without
  # whole-archiving a library or relying on the linker to pull unused members.
  allocate = memory: flags: package: package.overrideAttrs (old: {
    env = (old.env or {}) // { NIX_CFLAGS_COMPILE = "${isaFlags} ${flags}"; };
    preConfigure = (old.preConfigure or "") + ''
      cmakeFlagsArray+=( ${lib.escapeShellArg
        "-DCMAKE_EXE_LINKER_FLAGS=${memory}/lib/mimalloc.o -Wl,--emit-relocs ${flags}"} )
    '';
    dontStrip = true;
    separateDebugInfo = false;
  });
  mkLLVM = flags: let
    memory = if flags == "" then allocator else allocator.overrideAttrs (old: {
      env = (old.env or {}) // { NIX_CFLAGS_COMPILE = "${isaFlags} ${flags}"; };
    });
  in (llvm.override { inherit stdenv; }).overrideScope (final: prev: {
    libllvm = allocate memory flags (prev.libllvm.overrideAttrs (old: {
      cmakeFlags = old.cmakeFlags ++ [ "-DLLVM_TARGETS_TO_BUILD=Native" "-DLLVM_ENABLE_LTO=OFF" ];
    }));
    libclang = allocate memory flags (prev.libclang.override { libllvm = final.libllvm; });
    lld = allocate memory flags (prev.lld.override { libllvm = final.libllvm; });
  });
  subjectLLVM = mkLLVM "";
  cmake = (static.cmake.override {
    inherit stdenv;
    useSharedLibraries = false;
  }).overrideAttrs (old: {
    env = (old.env or {}) // {
      NIX_CFLAGS_COMPILE = isaFlags;
      NIX_LDFLAGS = "${allocator}/lib/mimalloc.o";
    };
    dontStrip = true;
    dontAddStaticConfigureFlags = true;
    configureFlags = lib.filter (f: f != "--disable-shared" && f != "--enable-static")
      (old.configureFlags or []);
  });
  ninja = (static.ninja.override { inherit stdenv; buildDocs = false; }).overrideAttrs (old: {
    env = (old.env or {}) // {
      NIX_CFLAGS_COMPILE = isaFlags;
      NIX_LDFLAGS = "${allocator}/lib/mimalloc.o";
    };
    dontStrip = true;
  });
  ccache = allocate allocator "" ((static.ccache.override { inherit stdenv; }).overrideAttrs (old: {
    nativeCheckInputs = (old.nativeCheckInputs or []) ++ [ pkgs.elfutils ];
    # pkgsStatic moves linked libraries into propagatedBuildInputs.
    propagatedBuildInputs = lib.filter
      (p: !(lib.hasPrefix "hiredis" (p.pname or ""))) old.propagatedBuildInputs;
    cmakeFlags = (old.cmakeFlags or []) ++ [ "-DREDIS_STORAGE_BACKEND=OFF" ];
  }));
  baseline = import ./bundle.nix {
    inherit pkgs gnu cmake ninja ccache;
    llvm = subjectLLVM;
  };
  checks = import ./eval-checks.nix { inherit pkgs stdenv allocator subjectLLVM cmake ninja ccache baseline; };
  trainingSource = llvm.libllvm.monorepoSrc;
  evaluation = assert checks; {
    inputs = {
      modern = { revision = nixpkgs.rev; hash = nixpkgs.narHash; };
      gnu231 = {
        revision = "cd63096d6d887d689543a0b97743d28995bc9bc3";
        hash = gnu231.narHash;
      };
    };
    derivations = lib.mapAttrs (_: p: p.drvPath) {
      inherit allocator baseline cmake ninja ccache;
    };
    versions = {
      llvm = llvm.libllvm.version;
      ccache = static.ccache.version;
      cmake = static.cmake.version;
      ninja = static.ninja.version;
      mimalloc = static.mimalloc.version;
      glibc = gnu.glibc.version;
      gcc = gnu.gcc.cc.version;
      training = llvm.libllvm.version;
    };
    compiler = stdenv.cc.name;
    host = stdenv.hostPlatform.config;
    isClang = stdenv.cc.isClang;
    isStatic = stdenv.hostPlatform.isStatic;
    trainingTarget = "LLVMSupport";
    sources = {
      nixpkgs = toString nixpkgs;
      gnu231 = toString gnu231;
      subject = toString llvm.libllvm.monorepoSrc;
      training = toString trainingSource;
    };
  };
in {
  inherit evaluation;
  checks.toolchain-eval = assert evaluation.isClang && evaluation.isStatic;
    pkgs.writeText "toolchain-evaluation.json"
      (builtins.unsafeDiscardStringContext (builtins.toJSON evaluation));
  packages = {
    toolchain-baseline = baseline;
  };
}
