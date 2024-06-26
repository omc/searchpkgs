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
  sd,
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

    # we provide the full distro and just the modules
    # modules are not distributed via maven repo so this is the only way to get at the necessary jars
    outputs = [ "out" "modules" ];

    nativeBuildInputs =
      [makeWrapper sd]
      ++ lib.optional stdenv.hostPlatform.isLinux autoPatchelfHook
      ++ lib.optional stdenv.hostPlatform.isDarwin fixDarwinDylibNames;

    buildInputs = [jdk17 util-linux zlib libxcrypt-legacy];

    runtimeDependencies = [zlib];

    installPhase = ''
      runHook preInstall

      mkdir -p $out
      cp -R bin config lib modules plugins $out

      # temporarily exclude some plugins until we can wire up the build
      # correctly for native artifacts
      rm -rf $out/plugins/x-pack-ml $out/modules/x-pack/x-pack-ml $out/modules/x-pack-ml

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

      # adapt default jvm options to only apply up to jdk11
      sd -- '^\-XX\:\+UseConcMarkSweepGC' "8-13:-XX:+UseConcMarkSweepGC" "$out/config/jvm.options"
      sd -- '^\-XX\:CMSInitiatingOccupancyFraction' "8-13:-XX:CMSInitiatingOccupancyFraction" "$out/config/jvm.options"
      sd -- '^\-XX\:\+UseCMSInitiatingOccupancyOnly' "8-13:-XX:+UseCMSInitiatingOccupancyOnly" "$out/config/jvm.options"

      runHook postInstall

      # copy just the modules
      mkdir $modules
      find $out/modules -type f -name '*.jar' -exec ln -s {} $modules/ \;

      mkdir -p $modules/nix-support
      cat << EOF > $modules/nix-support/setup-hook
      export ES_MODULES_JARS=$modules
      EOF
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
