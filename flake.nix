{
  description = "A collection of all versions of open-source search engines: OpenSearch, Apache Solr, Elasticsearch (Apache 2.0), Vespa";

  inputs = {
    nixpkgs.url = "nixpkgs";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = {
    nixpkgs,
    rust-overlay,
    ...
  }: let
    systems = ["aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux"];
    overlays = [rust-overlay.overlays.default];
    forAllSystems = f:
      nixpkgs.lib.genAttrs systems
      (system:
        f {
          pkgs = import nixpkgs {
            inherit overlays system;
          };
          inherit system;
          arch = builtins.elemAt (builtins.split system) 1;
        });
  in {
    packages = {
      aarch64-darwin = forAllSystems ({arch, ...}: {});
    };
    devShells = forAllSystems (
      {pkgs, ...}: {
        default = pkgs.mkShell {
          buildInputs = with pkgs;
            [
              (rust-bin.stable.latest.default.override {
                extensions = ["rust-analyzer" "rust-src"];
              })
            ]
            ++ pkgs.lib.optionals pkgs.stdenv.isDarwin
            (with pkgs.darwin.apple_sdk.frameworks; [Security SystemConfiguration]);
        };
      }
    );
  };
}
