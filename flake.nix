{
  description = "maintainer-mode - review a nixpkgs pull request, get yours on the list";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    treefmt-nix.url = "github:numtide/treefmt-nix";
  };

  outputs =
    {
      self,
      nixpkgs,
      treefmt-nix,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forAll = nixpkgs.lib.genAttrs systems;
      treefmtFor = system: treefmt-nix.lib.evalModule nixpkgs.legacyPackages.${system} ./treefmt.nix;
    in
    {
      packages = forAll (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        rec {
          maintainer-mode = pkgs.callPackage ./package.nix { };
          default = maintainer-mode;
        }
      );

      devShells = forAll (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            PRIMER_DIR = "${self.packages.${system}.maintainer-mode.primer}";
            packages = [
              pkgs.cargo
              pkgs.rustc
              pkgs.clippy
              pkgs.rustfmt
              pkgs.rust-analyzer
              pkgs.pkg-config
              pkgs.jq
              (treefmtFor system).config.build.wrapper
            ];
          };
        }
      );

      checks = forAll (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          build = self.packages.${system}.maintainer-mode;
          fmt = (treefmtFor system).config.build.check self;
          clippy = self.packages.${system}.maintainer-mode.overrideAttrs (old: {
            pname = "maintainer-mode-clippy";
            nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.clippy ];
            buildPhase = "cargo clippy --all-targets -- -D warnings";
            installPhase = "touch $out";
            doCheck = false;
          });
        }
      );

      formatter = forAll (system: (treefmtFor system).config.build.wrapper);
    };
}
