# Server rescans of a public flake

Status: done, off by default.

A person can register a public flake reference and an output on `/me/sets`.
After each nixpkgs index, the server evaluates that output again. It stores
the result as the person's set, with the same rules as an uploaded report.

This feature evaluates code that a third party wrote. Read this page before
you turn it on.

## What the server does

1. **Check the reference.** The server accepts two forms only:
   - `github:OWNER/REPO` or `github:OWNER/REPO/BRANCH`;
   - `https://HOST/PATH`, a tarball of a flake.

   The reference has no query and no fragment. The host is a DNS name under a
   public top-level domain. The server refuses an IP address, a port, a user,
   `localhost` and names such as `.local`, `.internal` and `.lan`. The output
   is an attribute path, such as
   `nixosConfigurations.HOST.config.system.build.toplevel`.

2. **Fetch the flake.** `nix flake prefetch` fetches the top-level tree only.
   It evaluates nothing.
3. **Check the lock file.** The tree must have a `flake.lock`. Each input in
   it must be one of these:
   - a `github`, `gitlab` or `sourcehut` input on the default host, locked to
     a commit;
   - a `tarball`, `file` or `git` input with an `https://` URL on a public
     host. A `git` input must not set `submodules` or `lfs`;
   - a `path` input inside the tree.

   The server refuses all other inputs, for example `ssh://`, `file://`,
   `indirect` and absolute paths.

4. **Fetch the inputs.** `nix flake archive --no-update-lock-file` fetches the
   inputs that the lock file names. Nix cannot add or change an input in this
   step. The reference is pinned to the hash of the tree that step 3 checked.
5. **Evaluate.** `nix derivation show -r --offline` reads the closure of the
   output. This step has no network.
6. **Filter.** The server matches each package name against its own nixpkgs
   index, as `scan` does on a client. Then it applies the rules of an upload:
   the same redaction, the same index filter, and the same 90-day rule.

## Threat model

The flake author controls `flake.nix`, the lock file and every input. The
author can try to:

- read the secrets of the service: the OAuth client secret, the GitHub App
  key, the sync token, the session tokens in the database;
- read or change the database, the event log or the sets of other people;
- reach the host, the local network or a cloud metadata service;
- use all the memory, CPU time or disk of the host;
- run a build, or make the server substitute a large closure;
- put a private name in a set.

The sandbox and the limits below answer each item.

## What the sandbox allows

Each of the three nix steps is a separate process. `prlimit` starts
`bwrap`, and `bwrap` starts `nix`.

| The sandboxed process                          | Can                                                       | Cannot                                   |
| ---------------------------------------------- | --------------------------------------------------------- | ---------------------------------------- |
| `/nix/store`                                   | read                                                      | write                                    |
| the nix daemon socket                          | send requests as the service user                         | use the privileges of a trusted user     |
| a new temporary directory, `/work`             | read and write                                            | outlive the scan: the server deletes it  |
| `/tmp`                                         | read and write a new empty tmpfs                          | see the `/tmp` of the service            |
| `/proc`, `/dev`                                | see its own processes and a minimal `/dev`                | see the processes of the host            |
| `/var/lib/maintainer-mode`, `/run/credentials` | nothing: they are not in the sandbox                      |                                          |
| environment                                    | `PATH`, `HOME` and the XDG directories inside `/work`     | read a token: the environment is cleared |
| network, steps 2 and 4                         | use the network of the host, `/etc/resolv.conf`, CA files |                                          |
| network, step 5                                | nothing: a new network namespace with only loopback       | fetch a file                             |
| user, IPC, UTS, cgroup namespaces              | its own                                                   | send a signal to the service             |
| capabilities                                   | none                                                      |                                          |

Nix runs with these settings:

- `allow-import-from-derivation = false`: the evaluation builds nothing.
- `accept-flake-config = false`: the `nixConfig` of the flake has no effect.
- `flake-registry = ""`: nix does not fetch the global registry.
- `--no-update-lock-file`: nix fetches only what the lock file names.
- `--offline` and `substitute = false` in step 5: the evaluation fetches and
  substitutes nothing.
- Pure evaluation, the default for flakes: the code cannot read the
  environment, `builtins.currentSystem` or a path outside the store.

The evaluation writes `.drv` files to the store through the nix daemon. The
daemon accepts them as it accepts them from any user that can use nix.

### The network of the fetch steps

A clean allowlist of hosts is not possible. The lock file can name any
public host, and bubblewrap shares the full network or none. Steps 2 and 4
therefore have the network of the host. The server limits what nix fetches
there:

- The server checks the reference and every input of the lock file before
  step 4, as step 3 says.
- The server refuses a `git` input that sets `submodules` or `lfs`. These
  make git fetch URLs that the lock file does not name.
- Nix fetches only the inputs of the lock file, because the lock file cannot
  change.
- These steps evaluate no output of the flake.

The checks look at the URL as it is written. They cannot stop these ways to
reach other addresses:

- A public DNS name can resolve to a private address.
- A public host can answer with an HTTP redirect. Nix follows redirects, to
  any address and over plain `http://`.

Thus a fetch step can reach every address that the host can reach. Do not
run the service on a host where such a fetch can do harm. Block the private
address ranges for the service, not only the metadata service of your cloud
provider. The sandbox runs in the cgroup of the service, so a rule on the
unit applies to it. For example, on NixOS:

```nix
systemd.services.maintainer-mode.serviceConfig.IPAddressDeny = [
  "10.0.0.0/8"
  "100.64.0.0/10"
  "169.254.0.0/16"
  "172.16.0.0/12"
  "192.168.0.0/16"
  "fc00::/7"
  "fe80::/10"
];
```

This rule applies to the whole service. Make sure that the service can still
reach its DNS resolver and its other servers.

## The limits

| Limit                         | Default  | Option                     |
| ----------------------------- | -------- | -------------------------- |
| flakes a person can register  | 3        | `rescan.maxFlakes`         |
| scans a person gets in 24 h   | 4        | `rescan.scansPerDay`       |
| wall clock of one scan        | 900 s    | `rescan.timeout`           |
| address space of each process | 3072 MiB | `rescan.memoryMB`          |
| CPU time of each process      | 900 s    | the same as the wall clock |
| size of one file it writes    | 4 GiB    |                            |
| standard output of one step   | 512 MiB  |                            |
| packages in a closure         | 50,000   | the same as an upload      |
| scans at the same time        | 1        |                            |

The server kills the sandbox at the limit. A failed scan keeps the
attributes of the last good scan. The panel shows the reason to the owner
only. The next try comes after the next nixpkgs index.

The scans run in their own task. A slow scan does not delay the sync of pull
requests and issues. `/sync` shows the number of flakes, the queue, the time
of the last scan and the number of failing flakes. It shows no reference,
login or package.

## What the server keeps

For each flake: the reference, the output, the time of the last scan, the
reason of the last failure, and from the last good scan the nixpkgs
attributes, the count of dropped entries, the time of the scan and the
nixpkgs revision of the lock file. Only the owner sees it. The attributes
expire 90 days after the last good scan. "Delete my set" deletes the flakes
too.

## Why it is off by default

- It runs code from people that the operator does not know. The sandbox
  lowers the risk; it does not remove it. A bug in nix, bubblewrap or the
  kernel can open a way out.
- The fetch steps have the network of the host. Only the operator knows what
  that network can reach.
- On NixOS, the service needs two relaxed settings. `ProtectKernelTunables`
  is off, because bubblewrap cannot mount a new `/proc` over the read-only
  parts of `/proc`. `RestrictAddressFamilies` adds `AF_NETLINK`, because
  bubblewrap sets up the loopback device of the new network namespace with
  it.
- A scan costs CPU time, memory and disk on the host. The service memory
  limit grows by `rescan.memoryMB`.
- A person can upload the same set from their own machine, with no risk to
  the server.

## Turn it on

On NixOS:

```nix
services.maintainer-mode.rescan.enable = true;
```

The module adds `bwrap`, `prlimit`, `nix` and `git` to the service. The host
must allow user namespaces for unprivileged users, as NixOS does by default.

Anywhere else, set `MAINTAINER_MODE_RESCAN=true` and give the paths in
`MAINTAINER_MODE_RESCAN_BWRAP`, `MAINTAINER_MODE_RESCAN_PRLIMIT` and
`MAINTAINER_MODE_RESCAN_NIX`. The nix must be plain nix, in `/nix/store`,
with a running nix daemon.
