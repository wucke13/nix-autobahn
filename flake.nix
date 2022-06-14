{
  inputs = {
    utils.url = "git+https://github.com/numtide/flake-utils";
    fenix.url = "git+https://github.com/nix-community/fenix?ref=main";
    fenix.inputs.nixpkgs.follows = "nixpkgs";
    naersk.url = "git+https://github.com/nix-community/naersk";
    naersk.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { self, nixpkgs, utils, naersk, fenix }:
    utils.lib.eachSystem [ "aarch64-linux" "i686-linux" "x86_64-linux" ]
      (system:
        let
          pkgs = import nixpkgs { inherit system; };
          rust-toolchain = fenix.packages.${system}.stable;
          naersk-lib = (naersk.lib.${system}.override {
            inherit (rust-toolchain)
              cargo
              rustc;
          })
          ;
        in
        rec {
          packages.default = naersk-lib.buildPackage {
            pname = "nix-autobahn";
            root = ./.;
            doCheck = true;
          };
          devShells.default = pkgs.mkShell {
            inputsFrom = [ packages.default ];
            nativeBuildInputs = with rust-toolchain; [ cargo clippy rustc rustfmt ];
          };
        });
}
