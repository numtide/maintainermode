{
  description = "maintainer-mode - review a nixpkgs pull request, get yours on the list";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    treefmt-nix.url = "github:numtide/treefmt-nix";
    iets = {
      url = "github:numtide/iets";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      treefmt-nix,
      iets,
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
          maintainer-mode = pkgs.callPackage ./package.nix {
            evaluator = iets.packages.${system}.classic or null;
          };
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
            # the full primer.css, so a new class works before the build trims it
            PRIMER_DIR = "${self.packages.${system}.maintainer-mode.primer.override { classes = null; }}";
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
        }
        // nixpkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
          vm = import ./nix/test.nix { inherit self pkgs; };
          instance =
            let
              node = nixpkgs.lib.nixosSystem {
                modules = [
                  self.nixosModules.instance
                  {
                    nixpkgs.hostPlatform = system;
                    boot.loader.grub.enable = false;
                    fileSystems."/" = {
                      device = "none";
                      fsType = "tmpfs";
                    };
                    system.stateVersion = "26.05";
                    security.acme.acceptTerms = true;
                    security.acme.defaults.email = "acme@example.org";
                    maintainermode = {
                      enable = true;
                      githubClientSecretFile = "/run/secrets/client-secret";
                      syncTokenFile = "/run/secrets/sync-token";
                      githubAppId = 1;
                      githubAppKeyFile = "/run/secrets/github-app-key";
                    };
                  }
                ];
              };
            in
            pkgs.writeText "instance-eval" (
              builtins.unsafeDiscardStringContext node.config.system.build.toplevel.drvPath
            );
        }
        // {
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

      nixosModules.default =
        { lib, pkgs, ... }:
        {
          imports = [ ./nix/module.nix ];
          services.maintainer-mode.package =
            lib.mkDefault
              self.packages.${pkgs.stdenv.hostPlatform.system}.maintainer-mode;
        };

      nixosModules.instance = {
        imports = [
          self.nixosModules.default
          ./nix/instance.nix
        ];
      };

      formatter = forAll (system: (treefmtFor system).config.build.wrapper);
    };
}
