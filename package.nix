{
  lib,
  rustPlatform,
  callPackage,
}:
let
  primer = callPackage ./nix/primer.nix { };
in
rustPlatform.buildRustPackage {
  pname = "review-club";
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.toml
      ./Cargo.lock
      ./src
    ];
  };

  cargoLock.lockFile = ./Cargo.lock;

  env.PRIMER_DIR = "${primer}";

  passthru = { inherit primer; };

  meta = {
    description = "Review a nixpkgs pull request, get yours on the list";
    mainProgram = "review-club";
    license = lib.licenses.mit;
    platforms = lib.platforms.unix;
  };
}
