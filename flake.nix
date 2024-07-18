{
  description = "A collection of all versions of open-source search engines: OpenSearch, Apache Solr, Elasticsearch (Apache 2.0), Vespa";

  inputs = {
    nixpkgs.url = "nixpkgs";
    rust-overlay.url = "github:oxalica/rust-overlay";
    nixel.url = "github:omc/nixel";
  };

  outputs = inputs @ {
    self,
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
            config.allowUnfree = true;
          };
          inherit system;
          arch = builtins.elemAt (builtins.split system) 1;
        });
  in {
    # TODO: study rust-overlay to figure out an API here
    overlays = {
      latest = final: prev: {
        opensearch = self.packages.${final.system}.opensearch_2_14_0;
        elasticsearch = self.packages.${final.system}.elasticsearch_8_13_4;
        quickwit = self.packages.${final.system}.quickwit_0_8_1;
      };
    };

    packages =
      (builtins.mapAttrs (
        system: systemPackages: let
          pkgs = import nixpkgs {
            inherit system;
            config.allowUnfree = true;
          };
        in (
          builtins.mapAttrs (_: {
            pname,
            version,
            url,
            sha256,
          }: let
            # The construction of the package is defined in a conventional location.
            packageDefinition = ./packages/${pname};
          in
            pkgs.callPackage packageDefinition {
              inherit pname version url sha256;
            })
          systemPackages
        ) //  (import ./packages/learning-to-rank/elasticsearch {
                     inherit pkgs inputs;
                     inherit (pkgs) lib;
                   })
      ) (builtins.fromJSON (builtins.readFile ./packages.json)));

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
