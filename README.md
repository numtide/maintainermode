# Maintainer Mode

**Tools that make it easier to contribute to nixpkgs.**

I ([zimbatm](https://github.com/zimbatm)) build Maintainer Mode for myself, to
make my maintenance of nixpkgs more enjoyable. It runs in public at
[maintainermode.org](https://maintainermode.org), so other contributors can
use it too. It is not an official NixOS project.

nixpkgs is big: more than 100,000 packages, 2,000 NixOS modules, thousands of
open pull requests and issues. Most of the friction is finding what needs
you, and knowing enough about it to act. Maintainer Mode reduces that
friction in three ways.

## 1. Navigate nixpkgs

Information about all of nixpkgs, for everyone, with no login:

- **A page for each package, NixOS module and directory**, with all the `meta`
  information, its maintainers and teams, its open issues and pull requests,
  and its health:
  - the newest upstream release, and the open version bumps;
  - advisories from the security tracker, with CVSS, EPSS and OSV;
  - the last Hydra build, or the NixOS test of a module;
  - the version in each channel, and when a fix needs a backport;
  - packaging gaps, and one risk score that sums it all up.
- **A page for each pull request**: its place in the queues, the packages it touches and their health, the diff, the reviews and a review checklist. You leave the review on GitHub.
- **Search** across packages, modules and directories. An exact name opens its page, and `@login` opens the page of a person.
- **Statistics:** the state of nixpkgs month by month (pull requests, reviews,
  merge times), and of its packages (outdated, vulnerable, unmaintained).
- **Queues:** pull requests that need review, and pull requests ready to merge.
- **A first package**: the home page lists small packages with no maintainer, each with one task to do.

## 2. Make it yours

Sign in with GitHub, and Maintainer Mode shows what you care about:

- **What you maintain**: every package and module whose `meta.maintainers` or
  `meta.teams` names you, worst first.
- **What you watch**: subscribe to any package, module or directory.
- **Your packages**: one list on the packages page. The packages of your machines come first, then what you maintain, then what you watch.
- **Needs you**: the top of the home page. It lists what waits for you, the most urgent first: a serious advisory on your machines, a new review on your pull request, a blocked pull request, a pull request to review. Snooze an item, or push Next to go through them.
- **Your pull requests**: set a follow-up plan on each one. If nobody
  reviews in some days, you get a reminder to ask the maintainers, or to
  merge it yourself when you are a committer. Maintainer Mode never merges.
- **Needs review, for you**: by default, the review queue shows the pull
  requests on what you maintain or watch.
- **Your system**: upload the closure of your machines, and follow the
  packages you really run. It stays private.
- **Notifications**: an inbox on the site and a private Atom feed for the
  reviews, merges and closes of your own pull requests, and for new
  pull requests, merges, issues, advisories and releases.
- **Your progress**: your reviews and your weekly goal. Only you see it.
- **For committers**: what is ready to merge in your areas, on the Ready to merge tab of the review page.
- **Onboarding**: three first steps. A person who maintains nothing yet learns how to start; a maintainer gets help to keep up.

## 3. Act on it

Each page links to the place on GitHub where you act: review the diff,
approve, ask for changes, or merge. Maintainer Mode tells you where to act;
you do the work on GitHub.

## New to nixpkgs?

Maintainer Mode helps you find work. nixpkgs decides how the work is done.
Read its own guides first:

- [CONTRIBUTING.md](https://github.com/NixOS/nixpkgs/blob/master/CONTRIBUTING.md):
  how to open, review and merge a pull request, commit messages, and the
  policy on automation and AI.
- [pkgs/README.md](https://github.com/NixOS/nixpkgs/blob/master/pkgs/README.md):
  how to write and update a package.
- [nixos/README.md](https://github.com/NixOS/nixpkgs/blob/master/nixos/README.md):
  how to write a NixOS module and its test.
- [maintainers/README.md](https://github.com/NixOS/nixpkgs/blob/master/maintainers/README.md):
  how to become a maintainer, and what it means.
- [The Nixpkgs manual](https://nixos.org/manual/nixpkgs/unstable/): the
  reference.

## Principles

- **It never writes to GitHub.** No reviews, no merges, no comments, no
  labels, no workflows.
- **It grants nothing.** Committers decide; Maintainer Mode shows evidence.
- **No ranking of people.** Reviews that move pull requests decide which
  pull requests get on the list. No page shows a score. See
  [the rules](docs/rules.md).
- **Your data stays yours.** Subscriptions, uploads and settings show only to
  you, and never in a public number.
- **Open about its state.** [Data status](https://maintainermode.org/state?tab=status) shows what
  the server has read and how fresh it is.

## Documentation

The site also shows these pages, in the Docs tab of
[/state](https://maintainermode.org/state?tab=docs).

| Page                                     | What it covers                                                 |
| ---------------------------------------- | -------------------------------------------------------------- |
| [docs/rules.md](docs/rules.md)           | the queues, routing, the state of nixpkgs (`/state?tab=rules`) |
| [docs/health.md](docs/health.md)         | package pages, the risk score, upstream, channels              |
| [docs/personal.md](docs/personal.md)     | what you maintain and watch, your package set                  |
| [docs/api.md](docs/api.md)               | the JSON API and the command line                              |
| [docs/data.md](docs/data.md)             | how the data is read, the event log, the archive               |
| [docs/operations.md](docs/operations.md) | run it, its variables, what we took                            |
| [docs/events.md](docs/events.md)         | the plan of the event log                                      |
| [docs/ingest.md](docs/ingest.md)         | the plan of the archive                                        |
| [docs/rescan.md](docs/rescan.md)         | server rescans of flakes, and their threat model               |

## Run it

```sh
nix build
./result/bin/maintainer-mode serve --db maintainer-mode.sqlite
```

The NixOS module is `nixosModules.default`; `nixosModules.instance` sets up
maintainermode.org. CI is `nix flake check`. More in
[docs/operations.md](docs/operations.md).

## Licence

MIT
