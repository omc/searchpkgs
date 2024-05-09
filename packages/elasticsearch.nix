{
  pname,
  version,
  url,
  sha256,
  stdenv,
  fetchurl,
}:
stdenv.mkDerivation {
  inherit pname version;
  src = fetchurl {
    inherit url sha256;
  };
  installPhase = "mkdir $out";
}
