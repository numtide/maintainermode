# Lists the outputs of a flake that a scan should read, when `--output` names
# none. The answer is the attribute path of each one, as a list of segments,
# so that a name with a dot or an `@` in it survives the way back.
#
# It forces names only, never a derivation: `builtins.attrNames` of the output
# sets, and the known path under each configuration. legacyPackages is all of
# nixpkgs and never belongs here.
#
# The packages of a devShell and of a check are yours to maintain like any
# other, so both go in. `--ignore` leaves an output or a whole kind out, and
# the scan does that with the answer.
#
# `runtime` is the `--runtime` of the scan, which reads built store paths.
# mkShell aborts the build of a shell on purpose, so a devShell has no store
# path to read and none goes. A check is a derivation like any other; one that
# this machine never built is skipped like any other output it cannot read.
let
  input = builtins.fromJSON (builtins.readFile ./input.json);
  flake = builtins.getFlake input.flake;

  # an output set that does not evaluate names nothing, instead of ending the
  # whole discovery
  names =
    set:
    let
      answer = builtins.tryEval (if builtins.isAttrs set then builtins.attrNames set else [ ]);
    in
    if answer.success then answer.value else [ ];

  bySystem =
    kind:
    map (n: [
      kind
      input.system
      n
    ]) (names (flake.${kind}.${input.system} or { }));
  configs =
    kind: suffix:
    map (
      n:
      [
        kind
        n
      ]
      ++ suffix
    ) (names (flake.${kind} or { }));

  toplevel = [
    "config"
    "system"
    "build"
    "toplevel"
  ];
in
bySystem "packages"
++ (if input.runtime then [ ] else bySystem "devShells")
++ bySystem "checks"
++ configs "nixosConfigurations" toplevel
++ configs "darwinConfigurations" toplevel
++ configs "homeConfigurations" [ "activationPackage" ]
