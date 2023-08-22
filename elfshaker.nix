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

  doCheck = true;
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
