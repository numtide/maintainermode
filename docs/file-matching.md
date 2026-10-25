# Match pull requests and issues by nixpkgs files

Status: design for review. `scan --files` is an experimental prototype. It
writes the file list into the local report only. The upload and the server do
not change yet.

## The problem

A person has flakes with `packages` and `nixosConfigurations` outputs. They
want to see only the pull requests and issues that affect those outputs.

Today the scan reads the derivation closure. Then it matches each derivation
back to an attribute by name: the pname, the lower-case name and
`python3Packages.<name>` (`PublicList::candidates` in `src/report.rs`). This
is fragile. A store path does not say which attribute or which file made it.

There is a better key. An evaluation reads nixpkgs files. A pull request
changes nixpkgs files. If the scan records the files that the evaluation read,
the server can match pull requests by file. It needs no name.

## What iets gives

iets is a Nix evaluator in Rust. Its evaluation cache records every
observation of the outside world that an evaluation makes. We use that record.
The facts below are from iets 0.1.0 at commit `0fba531c064e`.

- **The read set.** Each `import`, `readFile` and `.drv` read records the file
  with its size and hash. A copied tree (`./patch` coerced to a string,
  `builtins.path`, `filterSource`) records the tree. `pathExists` and
  `readDir` record a probe only. Source: `docs/explanation/caching.md`, "The
  tracing cache".
- **A CLI that prints it.** `iets eval` writes one cache entry per run.
  `iets cache dump ENTRY` prints its read set, one observation per line:
  `file PATH SIZE HASH`, `tree PATH accepted=all`, `stat …`, `dir …`. Source:
  `docs/reference/commands.md`, "cache"; `src/cli/cache_dump.rs`.
- **Flakes.** `iets eval` takes a locked local flake (`path:DIR#ATTR`), or
  `builtins.getFlake` under `--impure`. Remote flake references and IFD are not
  supported. Source: `README.md`, "Not supported (yet)";
  `docs/reference/builtins.md`.
- **A library.** The crate has `api::run::Run::cached` and
  `trace::run::Cache::entry_path()`. Source: `docs/reference/library.md`. But
  "the library never frees memory, so don't embed it in a long-running program
  yet" (`README.md`). The server must not link it.
- **Licence.** LGPL-2.1-or-later (`Cargo.toml`, `COPYING`). We call the binary
  as a separate process. Then the licence puts no rule on our code. Do not
  link the crate into this MIT program.

What iets does not give, and what the prototype does instead:

| Missing                                                                                                                                              | Workaround in the prototype                                                   | Ask                                              |
| ---------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------- | ------------------------------------------------ |
| No flag on `eval` that names its cache entry. `--entry-to FILE` exists on `print-dev-env` only.                                                      | A new, empty `--cache-dir` per scan; it holds one entry.                      | `eval --entry-to FILE`, or `--read-set-to FILE`. |
| `cache dump` is text for debugging, not a contract. A path with a space is ambiguous.                                                                | Parse from the right: the last two fields of a `file` line are size and hash. | `cache dump --json`.                             |
| `eval --json` fails on a list that `map` made: `cannot convert a thunk to JSON`. Reproduce with a flake output `x = builtins.map (a: a + 1) [ 1 ];`. | `builtins.toJSON` in the expression and `--raw`.                              | Fix the bug.                                     |
| `cache paths` leaves out store paths, so it leaves out a flake input.                                                                                | Use `cache dump`.                                                             | None.                                            |

## What the scan records

`scan --files` evaluates the chosen outputs once with iets. It keeps:

- each `file` observation under the nixpkgs input;
- each `tree` observation under the nixpkgs input (patches, builder scripts,
  `src = ./.` trees).

It drops `stat` and `dir` observations. They are probes. `pkgs/by-name` makes
a `dir` observation for every shard on every evaluation.

Each path is relative to the nixpkgs root, such as
`pkgs/servers/http/nginx/generic.nix`. The store prefix of nixpkgs comes from
`flake.inputs.<nixpkgs>.outPath` in the same evaluation. A path must be plain:
no leading `/`, no `.` or `..` part, no control character, at most 512 bytes.

The local report gets one new optional field, `files`:

```json
"files": {
  "files": ["lib/modules.nix", "pkgs/servers/http/nginx/generic.nix", "…"],
  "modules": ["nixos/modules/services/web-servers/nginx/default.nix", "…"],
  "modules_dropped": 2415
}
```

`modules` is the result of the module signal (below). `files` already has
the module filter applied.

## The NixOS module problem

A NixOS evaluation imports every file of `module-list.nix`. So the trace of an
nginx host has the Apache module too. In the test below, the trace has 2,465
paths under `nixos/modules/`, and a host without nginx has the same 2,465.

### The module signal

Keep a module file only when the configuration defines an option that the
file declares.

The module system records both sides:

- `options.<path>.declarations`: the files that declare the option;
- `options.<path>.files`: the files whose definitions survive `mkIf` and
  `mkOverride`.

`options.<path>.files` alone is not enough. To read it for every option, Nix
must evaluate every `mkIf` condition. Some conditions fail when the module is
off, with an error that `tryEval` does not catch. In the test,
`hardware.nvidia.open` stopped the walk with `expected a set but found null`.

So the signal works in two steps (`src/trace.nix`):

1. **Find candidates without the conditions.** Load each module of the
   configuration again with the real `config`. Walk its `config` attribute
   names down to an option. Do not go into the content of an `mkIf` whose
   condition is not `true`.
2. **Confirm with the module system.** Keep a candidate only when
   `options.<path>.files` names the file. This applies the real conditions,
   but only to options that the configuration uses.

Then it grows in hops:

- **Hop 0.** The options that your own files define. "Your own" is every file
  outside `nixos/modules/`. Keep the files that declare them.
- **Hop N.** The options that the module files of hop N−1 define. Keep the
  files that declare them. Stop when no file is new, or after 8 hops.

The client then keeps a path under `nixos/modules/` only when it is in the
area of a kept module. The area is the directory for a `default.nix`, as in
`area_key` (`src/area.rs`). So `nginx/vhost-options.nix` stays with
`nginx/default.nix`.

### Test result

The test host is a scratch flake on nixpkgs `7fd4004d0511`. It sets
`boot.loader.grub.device`, `fileSystems."/"`, `system.stateVersion` and
`services.nginx.enable = true`. A second host is the same without nginx.

|                           | nginx host | host without nginx |
| ------------------------- | ---------- | ------------------ |
| Hop 0                     | 8 files    | 7 files            |
| Hop 1                     | 31 files   | 24 files           |
| Fixed point (5 hops)      | 45 files   | 43 files           |
| Module paths in the trace | 2,465      | 2,465              |

- The nginx host keeps two module files that the other host does not:
  `services/web-servers/nginx/default.nix` and `security/acme` (the nginx
  module defines `security.acme.certs`).
- Apache, lighttpd and the other web servers are not kept.
- Hop 0 of the nginx host: `nginx/default.nix`, `loader/grub/grub.nix`,
  `boot/stage-1.nix`, `tasks/filesystems.nix`,
  `tasks/encrypted-devices.nix`, `tasks/filesystems/overlayfs.nix`,
  `misc/version.nix`, `misc/nixpkgs-flake.nix`.
- The later hops add core modules: `systemd.nix`, `users-groups.nix`,
  `etc.nix`, `top-level.nix`, `pam.nix`, `sudo.nix` and others.
- Some files declare an option inside a submodule of another module, such as
  `security/systemd-confinement.nix` for `systemd.services.<name>`. They are
  kept. That is correct: a change to them can change your service.

Known gaps:

- A submodule file that the declaring module imports by hand, such as
  `nginx/vhost-options.nix`, is not in `declarations`. The area rule keeps it.
- `nixosSystem` of the nixpkgs flake adds an inline module that sets
  `nixpkgs.flake.source`. It counts as your own file, so
  `misc/nixpkgs-flake.nix` is in hop 0.
- A `disabledModules` entry is not read. A disabled module cannot define an
  option, so it does not get through step 2.
- Options that only `lib.mkDefault` in a module set are found like any other
  definition. That is correct: the value comes from that module.

## Files that nearly every pull request touches

A trace has `lib/`, `pkgs/top-level/all-packages.nix`, `pkgs/stdenv/` and
`pkgs/build-support/`. Every evaluation reads them. A lens that matches on
them shows most of nixpkgs.

Measured on the nginx host, after the module signal (1,683 paths):

| Area                  | Paths |
| --------------------- | ----- |
| `pkgs/by-name/`       | 627   |
| `pkgs/development/`   | 494   |
| `pkgs/build-support/` | 109   |
| `nixos/modules/`      | 50    |
| `lib/`                | 43    |
| `nixos/lib/`          | 27    |
| `pkgs/top-level/`     | 18    |
| `pkgs/stdenv/`        | 15    |
| `*.patch`             | 285   |

Compare the raw traces, before the module filter: the nginx host reads 4,101
paths, and the host without nginx reads all but 14 of them. The 14 are the nginx
package files, its two patches, and `gixy`, `nginx-doc` and
`nginx-config-formatter`, which the nginx module uses. `hello` reads 381
paths, and the host without nginx reads 378 of them.

Proposal: the server sorts each matched path into one of three tiers. It uses
public data only. It never uses the sets of other people.

1. **Core.** The server traces a reference evaluation at each index: a
   minimal NixOS host and `hello`. Every path in that trace is core. A pull
   request that matches only core paths goes into a folded "Core" group. The
   lens shows its count, not its rows.
2. **Shared.** A path outside core that more than 25 packages share, by the
   same rule as `SHARED_AREA` in `src/links.rs`. Examples: a generic builder,
   `python-packages.nix`. The lens shows these rows below the specific ones.
3. **Specific.** All other paths. The lens shows these rows first.

The tier of a pull request is the best tier of its matched paths. Inside a
tier, the lens sorts by the number of specific paths matched, then by date.
`all-packages.nix` and `aliases.nix` are core, so a one-line alias change
does not reach the lens.

The server can learn more later. It can count how many pull requests touched
each path in 90 days. A path with a high count is a hub and goes to core.

## The privacy rule

A path leaves the machine only when it exists in public nixpkgs at the
revision that the scan read. This is the same strictness as the attribute
upload.

On the client:

- The nixpkgs input must be locked to `github:NixOS/nixpkgs` at a 40-character
  commit (`locked_nixpkgs` in `src/scan.rs`). Every file under that store
  path is then public at that commit. For a fork, a local path or a tarball,
  the client keeps no file and says so.
- The client keeps only paths under the nixpkgs store path. Your flake, your
  overlays and your other inputs never appear.
- The client sends the paths only. It sends no size, hash or store path.

On the server:

- It keeps a path only when the path exists in its nixpkgs tree at the index
  revision. The server already unpacks that tree (`area::download`). It
  builds a set of the paths once per index.
- It drops any other path and counts it, as it does for an unknown attribute.
  A path that a newer nixpkgs deleted is dropped too.
- It refuses an upload with more than 20,000 paths. The nginx host has 1,683.

## The upload format

The upload stays `schema: 1`. It gets one optional field, `files`:

```json
{
  "schema": 1,
  "source": { "system": "x86_64-linux", "nixpkgs_rev": "7fd4…", "…": "…" },
  "packages": ["…"],
  "omitted": { "unknown": 12 },
  "files": {
    "version": 1,
    "paths": ["lib/modules.nix", "pkgs/servers/http/nginx/generic.nix"],
    "omitted": 0
  }
}
```

- `files.version` lets the content change later without a new schema.
- An old client sends no `files`. The server keeps working as today.
- An old server refuses unknown fields (`deny_unknown_fields` on `Upload`). So
  a new client sends `files` only when the server says it accepts them. The
  server says so in `GET /api/v1/packages/index` with
  `"accepts": ["files/1"]`. Without it, the client leaves `files` out and
  prints a note.
- `submit` sends `files` only with `--files`, or when the local report has
  them. `export` shows them first, like the attributes.

## How the server matches

`src/links.rs` already links a pull request by the files it changes
(`Pr.files`), and an issue by the paths in its body (`Issue.mentions` with
`subject::is_path`). File matching adds a second lookup with the same input:

1. Keep the paths of the set in memory as a set of strings.
2. For a pull request, match each path of `Pr.files`. Match each parent
   directory too, so a new file in `pkgs/by-name/ng/nginx/` matches the
   directory of a file that the set read.
3. For an issue, match each path of `mentions` in the same way.
4. Give each match its tier (above).

Two limits apply:

- `Pr.files` holds the first 50 paths, or up to 10 000 on a server with the
  git copy ("Full file lists" in [data.md](data.md#the-git-copy)). A pull request with
  `changed_files` above 50 is a mass change. It goes to the Core group, unless
  one of its 50 paths is specific.
- Issues seldom name a path. The name match of today stays for issues. A file
  match adds to it and does not replace it.

The cost is small. One set has about 2,000 paths. The open pull requests have
fewer than 50 paths each. One pass over all open pull requests is a few
hundred thousand set lookups.

## Named sets

Another change adds named package sets, keyed by `(login, name)`, and public
flake sets on profiles. File matching follows the same keys:

- A file list belongs to one set, keyed by `(login, name)`. A new upload to a
  set replaces its file list, like its attributes.
- The store keeps the paths of a set as one compressed blob in the set row, not
  as one row per path. A set has about 2,000 paths. The server never queries
  one path across sets.
- The lens works per set: "pull requests that affect `web`". "All my sets" is
  the union.
- The same 90-day rule and the same delete button apply.
- A **public flake set** on a profile: the flake is public, so its file list
  is public data. The profile can show its lens. A private set never shows on
  a profile.
- A **server rescan** of a public flake (`docs/rescan.md`) can record the
  files too. It needs iets in the sandbox in place of `nix derivation show`.
  This waits until the client side has run for a while.

## Cost and memory

Measured on the nginx host with iets 0.1.0, cold IR cache:

| Step                                               | Wall time | Peak memory |
| -------------------------------------------------- | --------- | ----------- |
| `iets eval` of `toplevel.drvPath`, traced          | 3.9–4.5 s | 0.86 GB     |
| The same, with the module signal (`src/trace.nix`) | 3.3–9.2 s | 0.87 GB     |
| The module signal alone with Nix 2.36              | 6.5–8.6 s | 0.63 GB     |
| `hello.drvPath`, traced                            | 1.7 s     | 0.22 GB     |

- The read set of the nginx host has 5,261 observations. The cache entry and
  the dump are each below 1 MB.
- A real desktop host reads more modules and more packages. Expect 2 to 4
  times the time and memory. A scan is one evaluation per run, on the owner's
  machine.
- On the server, a reference trace per index costs about one host
  evaluation. Matching costs almost nothing.

## The prototype

- `src/trace.nix`: the evaluation and the module signal. It reads its input
  from `input.json`, as the other generated expressions do.
- `src/trace.rs`: runs `iets eval --impure --raw --cache-dir <new dir>`, finds
  the one cache entry, runs `iets cache dump`, keeps the nixpkgs paths and
  applies the module filter.
- `maintainer-mode scan --flake . --output … --files` writes the result into
  the `files` field of the local report. `export` and `submit` ignore it.
- It needs `iets` on `PATH`. It refuses a `--file` target and a remote flake.

Result on the nginx host:

```
$ maintainer-mode scan --flake . --files \
    --output nixosConfigurations.web.config.system.build.toplevel
1683 nixpkgs files, 45 modules kept, 2415 module files dropped
1481 packages -> maintainer-mode-scan.json
```

## Rollout

1. **Now.** `scan --files`, local only. Owners try it on their hosts. We
   collect the counts, the time and the memory.
2. **Server, read side.** Build the path set of each index and the core trace.
   Add `"accepts": ["files/1"]`. Accept `files` in uploads, check the paths,
   store the blob. Show nothing yet.
3. **The lens.** A "By files" filter on the set view, beside the name match.
   Compare both on our own sets for a few weeks.
4. **Default.** If the file match is better, `submit` sends `files` by default
   when iets is on `PATH`. The name match stays as the fallback for a scan
   without iets.
5. **Server rescans.** Use iets in the rescan sandbox for public flakes.

## Open questions

- Is the `github:NixOS/nixpkgs` rule too strict? A flake can take nixpkgs
  from a channel tarball. The server could accept those paths and check them
  against its own tree only.
- Do we ask iets for `eval --entry-to` and `cache dump --json`, or wait?
- Is iets a hard dependency of the client package, or an optional tool on
  `PATH`? The flake input would add its closure to `nix shell`.
- Should the module signal also run without iets, with plain Nix, for people
  who only want the module part?
- Hop 0 alone gives a very small set (8 files). The fixed point adds core
  modules. Do we keep the hops, or keep hop 0 and treat the rest as core?
- iets records the position of `pname` for each derivation that it creates
  (`docs/explanation/provenance.md`). That maps a derivation to its file
  directly. It could replace the name match for packages. Is that worth a
  second prototype?
