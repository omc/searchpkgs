# adapted from https://github.com/NixOS/nixpkgs/blob/master/pkgs/servers/search/elasticsearch/7.x.nix
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
}: let
  applyPatch = (builtins.compareVersions version "6.4.0") >= 0;
in
  stdenv.mkDerivation {
    inherit pname version;

    src = fetchurl {
      inherit url sha256;
    };

    patches = lib.optionals applyPatch [./es-home-6.x.patch];

    postPatch = ''
      if [ -e bin/elasticsearch-env ]; then
          substituteInPlace bin/elasticsearch-env --replace \
            "ES_CLASSPATH=\"\$ES_HOME/lib/*\"" \
            "ES_CLASSPATH=\"$out/lib/*\""
      fi

      if [ -e bin/elasticsearch-cli ]; then
        substituteInPlace bin/elasticsearch-cli --replace \
          "ES_CLASSPATH=\"\$ES_CLASSPATH:\$ES_HOME/\$additional_classpath_directory/*\"" \
          "ES_CLASSPATH=\"\$ES_CLASSPATH:$out/\$additional_classpath_directory/*\""
      fi
    '';

    nativeBuildInputs =
      [makeWrapper]
      ++ lib.optional stdenv.hostPlatform.isLinux autoPatchelfHook
      ++ lib.optional stdenv.hostPlatform.isDarwin fixDarwinDylibNames;

    buildInputs = [jdk17 util-linux zlib libxcrypt-legacy];

    runtimeDependencies = [zlib];

    installPhase = ''
      runHook preInstall

      ls -alr plugins
      mkdir -p $out
      cp -R bin config lib modules plugins $out

      chmod +x $out/bin/*

      substituteInPlace $out/bin/elasticsearch \
        --replace 'bin/elasticsearch-keystore' "$out/bin/elasticsearch-keystore"

      wrapProgram $out/bin/elasticsearch \
        --prefix PATH : "${lib.makeBinPath [util-linux coreutils gnugrep]}" \
        --set JAVA_HOME "${jdk17}" \
        --set ES_JAVA_HOME "${jdk17}"

      wrapProgram $out/bin/elasticsearch-plugin \
          --set JAVA_HOME "${jdk17}" \
          --set ES_JAVA_HOME "${jdk17}"

      runHook postInstall
    '';

    passthru = {enableUnfree = true;};

    meta = with lib; {
      description = "Open Source, Distributed, RESTful Search Engine";
      sourceProvenance = with sourceTypes; [
        binaryBytecode
        binaryNativeCode
      ];
      license = licenses.elastic20;
      platforms = platforms.unix;
    };
  }
