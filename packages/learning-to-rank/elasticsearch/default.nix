{
  inputs,
  pkgs,
  lib,
  ...
}: let
  pkgsExtended = pkgs.extend (import "${inputs.nixel}/nix/overlay");

  mkSrc = {
    version,
    hash ? "",
  }:
    pkgs.fetchFromGitHub {
      owner = "o19s";
      repo = "elasticsearch-learning-to-rank";
      rev = version;
      inherit hash;
    };

  versions = [
    rec {
      src = mkSrc {
        inherit version;
        hash = "sha256-Lx0ctZcKViqTwLbJ/xesQt0AJbhMyqoVliEnv8Xq2bs=";
      };
      version = "v1.5.9-es8.14.2";
    }
  ];

  packages =
    builtins.listToAttrs
    (builtins.map
      ({
        src,
        version,
      }: {
        name = "learning-to-rank-es_${builtins.replaceStrings ["."] ["_"] version}";
        value = pkgsExtended.nixelGen.mkDerivation {
          pname = "learning-to-rank-elasticsearch";
          inherit src version;
          lockFile = ./. + "/${version}.lock";
          inherit (pkgs) jdk gradle;
        };
      })
      versions);
in
  packages
