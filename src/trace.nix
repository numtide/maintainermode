# Evaluates the chosen outputs of a local flake, so that the evaluator's file
# trace holds every nixpkgs file they read, and computes the module signal of
# each NixOS configuration among them.
#
# The module signal: a module file of nixpkgs counts when the configuration
# defines an option that the file declares. Hop 0 starts from the options
# that files outside nixos/modules define. Each next hop adds the files that
# declare the options that the files of the last hop define.
#
# The result is a JSON string: `iets eval --json` cannot print a list that
# `map` made (0.1.0).
let
  input = builtins.fromJSON (builtins.readFile ./input.json);
  flake = builtins.getFlake input.flake;
  nixpkgs = flake.inputs.${input.nixpkgs};
  lib = nixpkgs.lib;
  prefix = toString nixpkgs.outPath + "/";
  inNixpkgs = f: lib.hasPrefix prefix (toString f);
  isModuleFile = f: lib.hasPrefix (prefix + "nixos/modules/") (toString f);
  rel = f: lib.removePrefix prefix (toString f);

  signal =
    system:
    let
      inherit (system) options config;
      special = system._module.specialArgs or { };
      margs =
        system._module.args
        // special
        // {
          inherit config options lib;
          modulesPath = prefix + "nixos/modules";
        };
      base = special // {
        inherit config options lib;
        _class = "nixos";
        _prefix = [ ];
      };
      call =
        m: if lib.isFunction m then m (builtins.intersectAttrs (lib.functionArgs m) margs // base) else m;
      isFile = m: builtins.isPath m || builtins.isString m;
      load =
        file: m:
        let
          v = call (if isFile m then import m else m);
          f = if isFile m then toString m else v._file or file;
          short = !(v ? config || v ? options);
          cfg =
            if short then
              removeAttrs v [
                "_file"
                "key"
                "imports"
                "disabledModules"
                "_class"
                "meta"
                "freeformType"
              ]
            else
              v.config or { };
        in
        [
          {
            file = f;
            config = cfg;
          }
        ]
        ++ lib.concatMap (load f) (v.imports or [ ]);

      isOpt = p: lib.isOption (lib.attrByPath p null options);
      walk =
        p: v:
        if builtins.isAttrs v && (v._type or null) == "if" then
          (
            let
              c = builtins.tryEval (v.condition == true);
            in
            if c.success && c.value then walk p v.content else [ ]
          )
        else if
          builtins.isAttrs v
          && builtins.elem (v._type or null) [
            "override"
            "order"
          ]
        then
          walk p v.content
        else if builtins.isAttrs v && (v._type or null) == "merge" then
          lib.concatMap (walk p) v.contents
        else if p != [ ] && isOpt p then
          [ p ]
        else if builtins.isAttrs v && (p == [ ] || lib.hasAttrByPath p options) then
          lib.concatLists (lib.mapAttrsToList (n: walk (p ++ [ n ])) v)
        else
          [ ];

      defined =
        mods:
        lib.unique (
          lib.concatMap (
            m:
            map (p: {
              inherit p;
              inherit (m) file;
            }) (walk [ ] m.config)
          ) mods
        );
      confirmed =
        d:
        let
          files = (lib.getAttrFromPath d.p options).files;
        in
        if isModuleFile d.file then builtins.elem d.file files else lib.any (f: !(isModuleFile f)) files;
      declared = d: map rel (builtins.filter inNixpkgs (lib.getAttrFromPath d.p options).declarations);

      own = builtins.filter (m: !(isModuleFile m.file)) (
        lib.concatMap (load "<config>") system._module.args.modules
      );
      hop0 = lib.unique (lib.concatMap declared (builtins.filter confirmed (defined own)));
      loadRel = f: builtins.filter (m: isModuleFile m.file) (load (prefix + f) (prefix + f));
      grow =
        n: seen: frontier:
        if n >= input.maxHops || frontier == [ ] then
          seen
        else
          let
            defs = builtins.filter confirmed (defined (lib.concatMap loadRel frontier));
            new = builtins.filter (f: !(builtins.elem f seen)) (lib.unique (lib.concatMap declared defs));
          in
          grow (n + 1) (seen ++ new) new;
    in
    {
      files = grow 0 hop0 hop0;
    };

  one =
    path:
    let
      value = lib.getAttrFromPath path flake.outputs;
      host =
        if builtins.length path >= 2 && builtins.head path == "nixosConfigurations" then
          flake.outputs.nixosConfigurations.${builtins.elemAt path 1}
        else
          null;
    in
    {
      # nothing reads it: forcing drvPath is what makes iets record the files
      # that the output depends on.
      drv = value.drvPath or null;
      modules = if host == null then null else signal host;
    };
in
builtins.toJSON {
  nixpkgs = toString nixpkgs.outPath;
  outputs = map one input.outputs;
}
