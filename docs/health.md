# Package health

What a package, module or area page shows, and where each fact comes from.

## Packages, modules and areas

Every package, NixOS module and nixpkgs path has a page: `/p/<attr>`,
`/m/<module file>` and `/a/<path>`. `/packages` searches them. Each page shows
health first, then the open issues and pull requests, then the ones closed or
merged in the synced window.

- **Health of a package:** its nixpkgs version and the newest upstream release
  (only when nixpkgs is behind; see [Upstream versions](#upstream-versions)), the last
  Hydra build of `nixpkgs/unstable/<attr>.x86_64-linux`, the open advisories
  of the [security tracker](https://tracker.security.nixos.org). An advisory
  counts as open while its nixpkgs issue is
  open. Each advisory shows the worst severity of its CVEs (CVSS), whether
  CISA saw an exploit, and its [EPSS](https://www.first.org/epss/) score: the
  chance of exploitation in the next 30 days. A package from PyPI,
  crates.io or npm also shows the [OSV](https://osv.dev) advisories for the
  exact version nixpkgs ships, unless a tracker advisory names the same CVE.
- **The Package box:** what nixpkgs says about the package, as the index sweep read it. It shows the description and the long description as plain text. The sweep keeps the first 500 characters of the description and the first 2000 characters of the long description. It shows the homepage, the download page and the changelog. It shows each license with its SPDX id, and marks unfree licenses. It shows a short platform summary (how many, and Linux or Darwin) and `meta.badPlatforms`. It also shows `meta.mainProgram`, `meta.sourceProvenance` and the outputs. It shows whether the package is broken, insecure (with the text of `meta.knownVulnerabilities`) or unfree. It shows the position, with a link to the file and line at the index revision. It ends with the maintainers and teams. The sweep reads only what is already in `meta`, so it builds nothing. An index swept before this has no such data, and the box says so. `GET /api/v1/packages/<attr>` gives the same under `package.meta`.
- **Version bumps:** an open pull request whose title is `<name>: <old> -> <new>` (or with `→`) is a version bump. Most come from r-ryantm. A `[Backport …]` prefix is allowed. The package page lists the open bumps of the package. The tab **Bumps** of `/review` (`/review?tab=bumps`) lists all open bumps, the oldest first, 50 on each page, with their checks, approvals, age and the newest upstream version. When you sign in, "what you maintain or watch" (`&filter=mine`) keeps only the bumps of the packages you maintain (directly or through a team), the packages you watch and the directories you watch. `GET /api/v1/bumps` gives the same; add `?filter=mine` with your session. Read [how nixpkgs reviews a package update](https://github.com/NixOS/nixpkgs/blob/master/pkgs/README.md#package-updates) before you approve one.
- **Packaging hygiene of a package:** cheap checks on what the index sweep
  reads, with no network and no build:
  - `meta.description`, `meta.homepage` and `meta.license` are set;
  - `meta.description` does not end with a period or start with an article;
  - a program has `meta.mainProgram` and runs `versionCheckHook`;
  - `meta.broken` and `meta.knownVulnerabilities` show as warnings.

  The sweep does not build, so "a program" is a guess: a top-level attribute
  under `pkgs/by-name`, `pkgs/applications`, `pkgs/tools`, `pkgs/servers` or
  `pkgs/games` whose name does not start with `lib`.

- **Health of a module:** the last Hydra build of its NixOS test
  (`nixos.tests.<name>`), and its maintainers and teams.
- **An area** lists the packages and modules under it, the ones that need
  attention first, its `ci/OWNERS` lines, and the busiest directories below it.
- **Pull requests** belong to a package or module by the files they change
  (its directory, or its NixOS test) and by the title convention:
  `<name>: …` or `nixos/<name>: …`. A file that more than 25 packages share,
  like `all-packages.nix`, links nothing.
- **Issues** belong by their title, with the forms of
  `.github/ISSUE_TEMPLATE` (`Build failure: <name>`, `Update Request: <name> …`),
  and by the nixpkgs paths and the "Package name" field in their body.

### Hydra

Maintainer Mode asks Hydra for the last build when someone opens a package or module page. To protect hydra.nixos.org, it sends at most 4 requests at a time and at most 30 requests a minute. Over that limit, the page shows "not asked yet" and sends no request. An answer stays one hour. A job with no finished build also stays one hour. A request that fails stays five minutes. `/sync` shows the limits and how many page views were not asked. The JSON API gives `hydra_state` (`test_state` for a module): `build`, `no finished build`, `no answer` or `not asked yet`.

## The risk score

Each package gets one number: how much it needs attention. The number is a
sum, and each part shows its reason. The package page lists the reasons.
On a list, the label shows them when you point at it. Lists show the
highest number first: the packages of an area, what you maintain, what you
watch, your package set, and the same lists in the JSON API (`risk`,
`risk_band`, `risk_reasons` in `flags`).

| Cause                                     | Points                                                         |
| ----------------------------------------- | -------------------------------------------------------------- |
| Each open advisory, by its worst CVE      | critical 40, high 25, medium 10, low 3, no score 5; 60 at most |
| One advisory exploited in the wild (CISA) | 20, or 8 for a public exploit                                  |
| The highest EPSS                          | 10 from 10 %, 4 from 1 %                                       |
| `meta.knownVulnerabilities`               | 15                                                             |
| `meta.broken`                             | 5                                                              |
| Behind upstream                           | 12 for a new major version, 8 otherwise                        |
| No maintainer and no team                 | 6                                                              |
| Packaging hygiene gaps                    | 1 each, 3 at most                                              |

The bands are: 0 ok, 1 to 14 low, 15 to 29 medium, 30 to 49 high, 50 and
more critical. The weights are a policy, not a measurement. They come from
nixfresh, simplified: there is no judgment of pull requests and no reading
of the git log.

## Upstream versions

The newest release comes from upstream itself when the source of a package
tells where to ask. [repology](https://repology.org) fills the rest.

- **Git tags.** When `src` comes from GitHub, a GitLab host (an archive URL
  or the API URL that fetchFromGitLab makes), Codeberg,
  sourcehut or KDE, Maintainer Mode asks the repository for its tags over
  HTTPS, as `git ls-remote --tags` does (protocol v2, one request, no token).
- **Registries.** When `src` comes from crates.io, PyPI or npm, the registry
  says which release is the newest.
- **Which tags count.** A tag counts when it names a version: `v1.2.3`,
  `openssl-3.5.0` and `curl-8_21_0` do; `nightly`, `v2.0-rc1` and
  `debian/1.2` do not. A prerelease (`alpha`, `beta`, `rc`, `dev` and the
  like) never counts. A tag must use the numbering scheme of the nixpkgs
  version: the same number of digits in the first part, and dots when
  nixpkgs writes dots. When some tags name the package, only those count.
- **Order.** Versions compare as nix compares them, except that a
  prerelease is older than its release.
- **Correction.** An answer from the tags or the registry overrules
  repology: it can show that nixpkgs is current. When the newest tag is
  older than nixpkgs, the tags were misread, and repology stays. A repology
  version counts only when it is newer, is no prerelease, and uses the same
  scheme.
- **Budget.** Each pass of the health task makes up to 300 requests, with
  100 ms between them. Packages that someone who signed in maintains, or
  that anyone watches, come first and are asked again after a day. The
  others are asked again after 7 days. A source that does not answer is
  asked again after a day. The wait doubles after each failure in a row,
  up to 32 days.

## Channels

A fix in master does not help users until their channel has it. Maintainer
Mode reads the channels next to master. The default list is `nixos-unstable`
and `stable`. `stable` is the newest stable channel on
[channels.nixos.org](https://channels.nixos.org). When that site does not
answer, the task tries again the next hour. It moves to an older release only
when the newer channel does not exist (404). When none exists, it is
`nixos-26.05`. Set the list with `--channel` (`MAINTAINER_MODE_CHANNELS`,
comma-separated) or with the NixOS option
`services.maintainer-mode.channels`.

- **What it keeps.** For each channel: its revision, the time it first saw
  that revision, and the version of each attribute. It keeps nothing else.
- **How it reads.** A separate task checks the revision of each channel
  every hour. When a revision changes, the task downloads that nixpkgs,
  evaluates only the version of each attribute in the swept sets, and deletes
  the source. It reads the channels one after the other. It never evaluates at
  the same time as the master sweep, so the peak memory stays near 2 GB. It
  waits while the master index is missing or older than 22 hours. It checks
  this again before each channel. It runs apart from the pull request sync.
- **The package page** has a "Channels" row: the version in master and in
  each channel. When master has a newer version, the row says which channels
  are behind. `GET /api/v1/packages/<attr>` gives the same under `channels`.
- **Needs backport.** An advisory whose nixpkgs issue closed in the synced
  window counts as fixed in master. When a stable channel has an older
  version than master, that advisory shows "needs backport". It stays on the
  page while a stable channel is behind. An open advisory gets no such hint:
  master can still be vulnerable. The API lists the fixed advisories under
  `needs_backport`.
- **What I maintain** marks each package that a channel has at an older
  version: "behind in …". The API gives `channels_behind`.
- **`/sync`** has one line per channel, with its revision and when it was
  first seen.
