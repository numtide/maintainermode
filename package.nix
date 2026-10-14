{
  lib,
  rustPlatform,
  callPackage,
  makeWrapper,
  # anything that answers `nix eval --json --file`; null leaves `nix` from PATH
  evaluator ? null,
}:
let
  primer = callPackage ./nix/primer.nix { };
in
rustPlatform.buildRustPackage {
  pname = "maintainer-mode";
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

  nativeBuildInputs = [ makeWrapper ];

  postInstall = lib.optionalString (evaluator != null) ''
    wrapProgram $out/bin/maintainer-mode \
      --set-default MAINTAINER_MODE_EVALUATOR ${evaluator}/bin/nix
  '';

  passthru = { inherit primer evaluator; };

  meta = {
    description = "What a nixpkgs maintainer should do next: review, merge, watch";
    mainProgram = "maintainer-mode";
    license = lib.licenses.mit;
    platforms = lib.platforms.unix;
  };
}
