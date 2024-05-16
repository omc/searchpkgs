# adapted from https://github.com/NixOS/nixpkgs/blob/master/pkgs/servers/search/elasticsearch/7.x.nix
{
  pname,
  version,
  url,
  sha256,
  lib,
  stdenv,
  fetchurl,
  jre_headless,
  util-linux,
  zlib,
  makeWrapper,
  coreutils,
  gnugrep,
  autoPatchelfHook,
}:
stdenv.mkDerivation {
  inherit pname version;

  src = fetchurl {
    inherit url sha256;
  };

  postPatch = ''
    substituteInPlace bin/elasticsearch-env --replace \
      "ES_CLASSPATH=\"\$ES_HOME/lib/*\"" \
      "ES_CLASSPATH=\"$out/lib/*\""

    substituteInPlace bin/elasticsearch-cli --replace \
      "ES_CLASSPATH=\"\$ES_CLASSPATH:\$ES_HOME/\$additional_classpath_directory/*\"" \
      "ES_CLASSPATH=\"\$ES_CLASSPATH:$out/\$additional_classpath_directory/*\""
  '';

  nativeBuildInputs =
    [makeWrapper]
    ++ lib.optional (!stdenv.hostPlatform.isDarwin) autoPatchelfHook;

  buildInputs = [jre_headless util-linux zlib];

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
      --set JAVA_HOME "${jre_headless}" \
      --set ES_JAVA_HOME "${jre_headless}"

    wrapProgram $out/bin/elasticsearch-plugin \
        --set JAVA_HOME "${jre_headless}" \
        --set ES_JAVA_HOME "${jre_headless}"

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
