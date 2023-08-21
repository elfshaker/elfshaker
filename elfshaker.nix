{
  naerskBuildPackage,
  rustToolchain,
  netcat,
  removeReferencesTo,
  stdenv,
}:

let
  target =
    if stdenv.hostPlatform.config == "x86_64-w64-mingw32"
    # Rust spells the target differently.
    then "x86_64-pc-windows-gnu"
    else stdenv.hostPlatform.config;
in

naerskBuildPackage target {
  name = "elfshaker-${stdenv.hostPlatform.config}";
  root = ./.;

  # Check is broken on x86 musl with:
  # >   = note: /nix/store/hjmy3kzf3cv15argfc10mgml1hbnyzph-x86_64-unknown-linux-musl-binutils-2.40/bin/x86_64-unknown-linux-musl-ld: /nix/store/cdaf8ha86l89d0nbfwhm42sg1q4jziic-x86_64-unknown-linux-musl-stage-final-gcc-12.2.0-lib/x86_64-unknown-linux-musl/lib/libstdc++.a(compatibility.o): relocation R_X86_64_32 against symbol `__gxx_personality_v0' can not be used when making a PIE object; recompi le with -fPIE
  # >           /nix/store/hjmy3kzf3cv15argfc10mgml1hbnyzph-x86_64-unknown-linux-musl-binutils-2.40/bin/x86_64-unknown-linux-musl-ld: failed to set dynamic section sizes: bad value
  doCheck = stdenv.hostPlatform.config != "x86_64-unknown-linux-musl";
  # doCheck = true;
  strictDeps = true;

  nativeBuildInputs = [
    stdenv.cc
    removeReferencesTo
    netcat # For tests.
    # TODO: Windows pthreads?
  ];

  postInstall = ''
    echo "Removing references to $cratesio_sources"
    remove-references-to -t $cratesio_sources $out/bin/elfshaker

    echo "Testing elfshaker binary with check.sh"
    $SHELL ${./.}/test-scripts/check.sh $out/bin/elfshaker ${./.}/test-scripts/artifacts/verification.pack
  '';
}
