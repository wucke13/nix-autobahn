{
  inputs = {
    utils.url = "github:numtide/flake-utils";
    naersk = {
      url = "github:nmattia/naersk";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    nixpkgs.url = "nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs, utils, naersk }:
    utils.lib.eachSystem [ "aarch64-linux" "i686-linux" "x86_64-linux" ]
      (system:
        let
          pkgs = import nixpkgs { inherit system; };
          naersk-lib = naersk.lib.${system};
        in
        rec {
          packages.nix-autobahn = naersk-lib.buildPackage {
            pname = "nix-autobahn";
            root = ./.;
            doCheck = true;
            doDoc = true;
            doDocFail = true;
          };
          defaultPackage = packages.nix-autobahn;
        });
}
