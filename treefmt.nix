{
  projectRootFile = "flake.nix";
  programs.nixfmt.enable = true;
  programs.rustfmt.enable = true;
  programs.prettier.enable = true;
  programs.taplo.enable = true;
  # Pages recorded from other sites stay as they came.
  settings.formatter.prettier.excludes = [ "src/testdata/*" ];
}
