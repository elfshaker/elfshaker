{
  lib,
  buildPackages,

  naerskBuildPackage,
  netcat,
  removeReferencesTo,
  rustToolchain,
  stdenv,
}:

let
  target =
    if stdenv.hostPlatform.config == "x86_64-w64-mingw32"
    # Rust spells the target differently.
    then "x86_64-pc-windows-gnu"
    else stdenv.hostPlatform.config;

  # Note for native builds, buildPackages.buildPackages == buildPackages; but
  # this is not so for cross (buildPackages has a different target in the cross-triple).
  nativePackages = buildPackages.buildPackages;

  # A script which runs elfshaker. On Windows this requires wrapping the binary
  # with a wine invocation, but the wrapper needs to know what to invoke. Get it
  # from the environment variables $ELFSHAKER_BIN.

  isWindows = target == "x86_64-pc-windows-gnu";

  elfshakerBinRunner = # "$ELFSHAKER_BIN";
    if !isWindows
    then "$ELFSHAKER_BIN"
    else nativePackages.writeShellScript "wine-wrapper" ''
      export WINEPREFIX="/tmp/elfshaker_testing" WINEDEBUG=-all
      export HOME=$WINEPREFIX FONTCONFIG_PATH=${nativePackages.fontconfig.out}/etc/fonts/
      mkdir -p $WINEPREFIX
      exec ${nativePackages.wine64}/bin/wine64 "''${ELFSHAKER_BIN}.exe" "$@"
  # '';

  maybeWindows = lib.optionalString isWindows "SKIP_BAD_WINDOWS_TESTS=1";
  packName = if isWindows
    then "verification-windows.pack"
    else "verification.pack";
in

naerskBuildPackage target {
  name = "elfshaker-${stdenv.hostPlatform.config}";
  root = ./.;

  strictDeps = true;

  nativeBuildInputs = [
    stdenv.cc
    removeReferencesTo
    netcat # For tests.
  ];

  postInstall = ''
    echo "Removing references to $cratesio_sources"
    remove-references-to -t $cratesio_sources $out/bin/elfshaker${lib.optionalString isWindows ".exe"}
  '';

  overrideMain = {
    doCheck = true;
    doInstallCheck = true;

    installCheckPhase = ''
      ${buildPackages.tree}/bin/tree $out
      echo "Testing elfshaker binary with check.sh"
      export ELFSHAKER_BIN=$out/bin/elfshaker
      ${maybeWindows} "$SHELL" ${./.}/test-scripts/check.sh ${elfshakerBinRunner} ${./.}/test-scripts/artifacts/${packName}
    '';
  };
}
