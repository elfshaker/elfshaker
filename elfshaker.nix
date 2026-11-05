{
  lib,
  buildPackages,

  naerskBuildPackage,
  netcat,
  openssl,
  removeReferencesTo,
  rustToolchain,
  stdenv,
}:

let
  target =
    if stdenv.hostPlatform.config == "x86_64-w64-mingw32"
    # Rust spells the target differently.
    then "x86_64-pc-windows-gnu"
    else if stdenv.hostPlatform.config == "arm64-apple-darwin"
    then "aarch64-apple-darwin" # Current rust toolchain doesn't understand the triple.
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
      # Wine decodes Unix command-line arguments using the locale code page.
      ${lib.optionalString nativePackages.stdenv.hostPlatform.isLinux ''
        export LOCALE_ARCHIVE=${nativePackages.glibcLocalesUtf8}/lib/locale/locale-archive
      ''}
      export HOME=$WINEPREFIX FONTCONFIG_PATH=${nativePackages.fontconfig.out}/etc/fonts/
      export XDG_RUNTIME_DIR="$WINEPREFIX/xdg-runtime"
      mkdir -p "$XDG_RUNTIME_DIR"
      chmod 0700 "$XDG_RUNTIME_DIR"
      exec env LC_ALL=en_US.UTF-8 ${nativePackages.wine64}/bin/wine "''${ELFSHAKER_BIN}.exe" "$@"
  # '';

  maybeWindows = lib.optionalString isWindows "SKIP_BAD_WINDOWS_TESTS=1";
  packName = if isWindows
    then "verification-windows.pack"
    else "verification.pack";
in

naerskBuildPackage isWindows target {
  name = "elfshaker-${stdenv.hostPlatform.config}";
  root = ./.;

  strictDeps = true;

  # Nix blocks the xattr writes Wine uses for Windows reparse points.
  # Exercise rejection without mutation here; native CI requires symlinks.
  ELFSHAKER_TEST_NO_SYMLINKS = lib.optionalString isWindows "1";
  cargoTestOptions = options:
    options ++ lib.optionals isWindows [ "--" "--skip" "requires_symlinks" ];

  nativeBuildInputs = [
    stdenv.cc
    removeReferencesTo
    netcat # For tests.
    openssl # For the local HTTPS clone test.
  ] ++ lib.optionals (!isWindows) [
    buildPackages.python3 # For the ESI HTTP fixture.
  ];

  postInstall = ''
    mkdir -p $out/share/man/man1
    install -m 0444 man/*.1 $out/share/man/man1/

    echo "Removing references to $cratesio_sources"
    remove-references-to -t $cratesio_sources $out/bin/elfshaker${lib.optionalString isWindows ".exe"}
  '';

  overrideMain = {
    doCheck = true;
    doInstallCheck = true;

    installCheckPhase = ''
      # Nix supplies an unusable CA path in sandboxed builds. The integration
      # suite uses local servers and sets its own explicit TLS overrides.
      unset SSL_CERT_FILE SSL_CERT_DIR
      ${buildPackages.tree}/bin/tree $out
      echo "Testing elfshaker binary with check.sh"
      export ELFSHAKER_BIN=$out/bin/elfshaker
      ${maybeWindows} ${buildPackages.coreutils}/bin/timeout \
        --signal=TERM --kill-after=30s 10m \
        "$SHELL" ${./.}/test-scripts/check.sh ${elfshakerBinRunner} ${./.}/test-scripts/artifacts/${packName}
      ${lib.optionalString (!isWindows) ''
        echo "Testing ESI commands and remote index compatibility"
        ${buildPackages.coreutils}/bin/timeout \
          --signal=TERM --kill-after=30s 10m \
          "$SHELL" ${./.}/test-scripts/check-esi.sh \
          "$ELFSHAKER_BIN" ${./.}/test-scripts/artifacts/${packName}
      ''}
    '';
  };
}
