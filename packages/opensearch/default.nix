{
  pname,
  version,
  url,
  sha256,
  lib,
  stdenv,
  fetchurl,
  jdk17,
  util-linux,
  zlib,
  makeWrapper,
  coreutils,
  gnugrep,
  autoPatchelfHook,
  libxcrypt-legacy,
  fixDarwinDylibNames,
  sd,
}: let
  # applyPatch = false;
in
  stdenv.mkDerivation {
    inherit pname version;
    src = fetchurl {
      inherit url sha256;
    };

    # patches = lib.optionals applyPatch [];

    # postPatch = ''
    # '';

    nativeBuildInputs =
      [makeWrapper sd]
      ++ lib.optional stdenv.hostPlatform.isLinux autoPatchelfHook
      ++ lib.optional stdenv.hostPlatform.isDarwin fixDarwinDylibNames;

    buildInputs = [jdk17 util-linux zlib libxcrypt-legacy];
    runtimeDependencies = [zlib];

    installPhase = ''
      runHook preInstall

      mkdir $out
      cp -R bin config lib modules plugins $out

      # temporarily exclude some plugins
      # tbd

      chmod +x $out/bin/*

      wrapProgram $out/bin/opensearch \
        --prefix PATH : "${lib.makeBinPath [util-linux coreutils gnugrep]}" \
        --set JAVA_HOME "${jdk17}" \
        --set OPENSEARCH_JAVA_HOME "${jdk17}"

      wrapProgram $out/bin/opensearch-plugin \
          --set JAVA_HOME "${jdk17}" \
          --set OPENSEARCH_JAVA_HOME "${jdk17}"

      runHook postInstall
    '';
  }
