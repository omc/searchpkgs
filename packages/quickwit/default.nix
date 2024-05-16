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
  buildPhase = '''';
  installPhase = ''
    mkdir $out
    mv * $out
  '';
}
