with import <nixpkgs> {};

stdenv.mkDerivation {
  name = "rust-env";

  buildInputs = [
    gcc musl rustup
    ];

  shellHook = ''
    NIX_ENFORCE_PURITY=0
  '';
}
