# Your own view

What Maintainer Mode shows for you: what you maintain, what you watch, and
the packages of your own system.

## What I maintain

`/u/<login>/maintains` lists the packages and NixOS modules whose
`meta.maintainers` names the login, or whose `meta.teams` names a team the
login is in. Each row carries the same health flags as a page. The highest
[risk score](health.md#the-risk-score) comes first. The list comes from nixpkgs;
change it there.

## What I watch

Subscribe to any package, module or area from its page, by name on `/watch`,
or to everything you maintain in one step. `/watch` shows each subscription
with its flags, the ones that need attention first.

- **The Atom feed** is the delivery. `/watch` makes a private link,
  `/feed/<token>`, with the last 30 days of what happened to your
  subscriptions: pull requests opened, merged or closed, issues opened or
  closed, and advisories, when the tracker opens their nixpkgs issue. An
  advisory with no nixpkgs issue shows on the page, not in the feed.
  Replacing the link cuts the old one.
- Subscriptions are private: they show only to you and in your feed. The
  package pages are public.
- **Why here.** Accounts, stored preferences and outbound messages are a write
  path. Maintainer Mode already logs people in and keeps state, so a
  subscription is one more row in its database.

## My package set

Tell Maintainer Mode which nixpkgs packages you run, and your feed tells you
when one of them gets an advisory or a new upstream release. The set is
private. Facts about nixpkgs are public; facts about your machines are not.

You evaluate your own tree on your own machine. By default, the server never
evaluates it. Three client commands do the work:

```sh
maintainer-mode scan --flake . --output nixosConfigurations.<host>.config.system.build.toplevel
maintainer-mode export --out report.json
maintainer-mode submit --to https://<host> --token-file ~/.config/maintainer-mode/upload-token
```

- **`scan`** reads the closure of one or more flake outputs (`--flake`,
  `--output`), or of the attributes of a file (`--file`, `--attr`), with
  `nix derivation show -r`. `--runtime` reads the built store paths with
  `nix path-info -r` instead. It matches each package name to a nixpkgs
  attribute: the name, the name in lower case, or `python3Packages.<name>`.
  It uses the nixpkgs that the tree pins in `flake.lock` or in
  `npins/sources.json`. `--nixpkgs-input` names the pin, and `--nixpkgs` gives
  a path or a flake reference. It writes the full report to
  `maintainer-mode-scan.json`, which stays on your machine.
- **`export`** writes the redacted report. It keeps only the packages with a
  nixpkgs attribute. It counts the other names and does not list them. It
  removes the name of the scanned tree. Read it before you send it.
- **`submit`** sends the redacted report to `POST /api/v1/reports`. It reads
  the upload token from `MAINTAINER_MODE_UPLOAD_TOKEN`, or from the file of
  `--token-file`.

The report format is the nixfresh report format, schema 1.

**The upload token.** Make it on `/me/sets`. The server shows it once and
keeps only its SHA-256 hash. A new token replaces the old one. One button
revokes it.

**What the server does with a report.**

| Code | Meaning                                              |
| ---- | ---------------------------------------------------- |
| 202  | stored; the answer gives the kept and dropped counts |
| 400  | an unreadable report or a schema other than 1        |
| 401  | no token, or an unknown token                        |
| 413  | more than 16 MiB, or more than 50,000 packages       |
| 503  | the nixpkgs index is not built yet                   |

- It keeps an attribute only when its own nixpkgs index has it. It drops all
  other entries. A broken or hostile client cannot store a private name.
- It keeps one set per login: a new report replaces the old one.
- It keeps the attributes, the time of the upload, the time of the scan and
  the nixpkgs revision. It keeps nothing else from the report.
- **Lifetime.** A set expires 90 days after its last upload. The background
  loop deletes it. "Delete my set" on `/me/sets` deletes it at once.
- **Access.** Only the owner sees `/me/sets`. There is no admin view. No
  public page, count or API uses an uploaded set.

**The panel.** `/me/sets` shows the attributes of your latest report and the
packages you watch. Each row has the health flags of a package page:
advisories, a newer upstream release, open pull requests and open issues.
The rows that need attention come first. The package page has the last Hydra
build.

**Alerts.** Your private Atom feed (see above) also gets, for each package in
your set, a new advisory and a new upstream release. It does not get the pull
requests and issues of the set, because a closure has thousands of packages.

**A public flake, scanned by the server.** This is off by default. When the
operator turns it on, `/me/sets` has a form for a public flake reference
(`github:OWNER/REPO`, `github:OWNER/REPO/BRANCH`, or an `https://` tarball)
and an output. After each nixpkgs index, the server scans the output again.
It keeps the result with the rules of an upload: the same redaction, the
same index filter and the same 90-day rule. The server stops the scans of a
person who has not opened `/me/sets` for 90 days. The rows of the panel then
show "in a scanned flake".

- The scan evaluates third-party code. Each nix step runs in a bubblewrap
  sandbox, as a separate process, with a read-only store, its own temporary
  directory, no service state, no secret, and a memory and time limit.
- The evaluation step has no network. The two fetch steps have the network
  of the host, and fetch only the inputs that a checked `flake.lock` names.
- Nix runs with `allow-import-from-derivation = false`: nothing is built.
- A person has at most 3 flakes and 4 scans a day, by default.
- `/sync` shows the flakes, the queue, the last scan and the failures, as
  counts only.

[`docs/rescan.md`](rescan.md) has the threat model, the sandbox, the
limits, and why the feature is off by default.
