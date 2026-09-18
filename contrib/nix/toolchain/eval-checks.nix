{ pkgs, stdenv, allocator, subjectLLVM, cmake, ninja, ccache, baseline }:
let
  inherit (pkgs) lib;
  tools = [ subjectLLVM.libllvm subjectLLVM.libclang subjectLLVM.lld cmake ninja ccache ];
  cmakeTools = [ subjectLLVM.libllvm subjectLLVM.libclang subjectLLVM.lld ccache ];
in
assert stdenv.cc.isClang;
assert stdenv.cc.version == pkgs.llvmPackages_23.llvm.version;
assert stdenv.hostPlatform.isStatic;
assert stdenv.hostPlatform.config == "aarch64-unknown-linux-musl";
assert lib.filter (lib.hasPrefix "-DLLVM_TARGETS_TO_BUILD=") subjectLLVM.libllvm.cmakeFlags
  == [ "-DLLVM_TARGETS_TO_BUILD=Native" ];
assert lib.all (p: p.stdenv.cc.isClang) tools;
assert lib.all (p: lib.hasPrefix "-march=armv8-a" p.NIX_CFLAGS_COMPILE) tools;
assert lib.all (p: lib.hasInfix
  (builtins.unsafeDiscardStringContext
    "-DCMAKE_EXE_LINKER_FLAGS=${allocator}/lib/mimalloc.o -Wl,--emit-relocs ")
  p.preConfigure) cmakeTools;
assert ninja.NIX_LDFLAGS == "${allocator}/lib/mimalloc.o";
assert cmake.NIX_LDFLAGS == "${allocator}/lib/mimalloc.o";
assert builtins.elem "-DMI_NO_OPT_ARCH:BOOL=TRUE" allocator.cmakeFlags;
assert builtins.elem "-DREDIS_STORAGE_BACKEND=OFF" ccache.cmakeFlags;
assert lib.all (p: !(lib.hasPrefix "hiredis" (p.pname or "")))
  (ccache.buildInputs ++ ccache.propagatedBuildInputs);
assert lib.hasInfix "--dynamic-linker=/lib/ld-linux-aarch64.so.1" baseline.buildCommand;
assert !(lib.hasInfix "\n-static\n" baseline.buildCommand);
assert subjectLLVM.libclang.libllvm.drvPath == subjectLLVM.libllvm.drvPath;
true
