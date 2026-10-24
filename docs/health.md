# Package health

What a package, module or area page shows, and where each fact comes from.

## Packages, modules and areas

Every package, NixOS module and nixpkgs path has a page: `/p/<attr>`,
`/m/<module file>` and `/a/<path>`. [Search](#search) finds them. Each page shows
health first, then the open issues and pull requests, then the ones closed or
merged in the synced window.

- **Health of a package:** its nixpkgs version and the newest upstream release
  (only when nixpkgs is behind; see [Upstream versions](#upstream-versions)), the last
  Hydra build of `nixpkgs/unstable/<attr>.x86_64-linux`, the open advisories
  of the [security tracker](https://tracker.security.nixos.org). An advisory
  counts as open while its nixpkgs issue is
  open. Each open advisory also shows its [verdict](#affected-versions):
  "affects" the master version, "range unknown", fixed in master while a
  channel still has an affected version, or "fixed in X · the tracker issue
  #N can be closed". Each advisory shows the worst severity of its CVEs (CVSS), whether
  CISA saw an exploit, and its [EPSS](https://www.first.org/epss/) score: the
  chance of exploitation in the next 30 days. A package from PyPI,
  crates.io or npm also shows the [OSV](https://osv.dev) advisories for the
  exact version nixpkgs ships, unless a tracker advisory names the same CVE.
- **The Package box:** what nixpkgs says about the package, as the index sweep read it. It shows the description and the long description as plain text. The sweep keeps the first 500 characters of the description and the first 2000 characters of the long description. It shows the homepage, the download page and the changelog. It shows each license with its SPDX id, and marks unfree licenses. It shows a short platform summary (how many, and Linux or Darwin) and `meta.badPlatforms`. It also shows `meta.mainProgram`, `meta.sourceProvenance` and the outputs. It shows whether the package is broken, insecure (with the text of `meta.knownVulnerabilities`) or unfree. It shows the position, with a link to the file and line at the index revision. It ends with the maintainers and teams. The sweep reads only what is already in `meta`, so it builds nothing. An index swept before this has no such data, and the box says so. `GET /api/v1/packages/<name>` gives the same under `package.meta`.
- **Version bumps:** an open pull request whose title is `<name>: <old> -> <new>` (or with `→`) is a version bump. Both `<old>` and `<new>` must contain a digit, so `lib.types: attrs -> attrsOf` is not a bump. Most come from r-ryantm. A `[Backport …]` prefix is allowed. The package page lists the open bumps of the package. The tab **Bumps** of `/review` (`/review?tab=bumps`) lists all open bumps, the oldest first, 50 on each page, with their checks, approvals, age and the newest upstream version. When you sign in, "what you maintain or watch" (`&filter=mine`) keeps only the bumps of the packages you maintain (directly or through a team), the packages you watch and the directories you watch. `GET /api/v1/prs?queue=bumps` gives the same; add `?filter=mine` with your session. Read [how nixpkgs reviews a package update](https://github.com/NixOS/nixpkgs/blob/master/pkgs/README.md#package-updates) before you approve one.
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

Maintainer Mode asks Hydra for the last build when someone opens a package or module page. To protect hydra.nixos.org, it sends at most 4 requests at a time and at most 30 requests a minute. Over that limit, the page shows "not asked yet" and sends no request. An answer stays one hour. A job with no finished build also stays one hour. A request that fails stays five minutes. `/state?tab=status` shows the limits and how many page views were not asked. The JSON API gives `hydra_state` (`test_state` for a module): `build`, `no finished build`, `no answer` or `not asked yet`.

The page asks `/api/latestbuilds?nr=30&project=…&jobset=…&job=…`: one request of about 6 KB for each jobset. It shows the newest of these builds that was not aborted or cancelled. The time is the time Hydra queued the build: the API gives no finish time.

**The channels.** A package page shows one Hydra row for master and one for each channel that the server follows (see [operations.md](operations.md)). Each channel has its jobset:

| Channel                                      | Hydra jobset                | Job of `hello`               |
| -------------------------------------------- | --------------------------- | ---------------------------- |
| master, `nixos-unstable`, `nixpkgs-unstable` | `nixpkgs/unstable`          | `hello.x86_64-linux`         |
| `nixos-YY.MM`                                | `nixos/release-YY.MM`       | `nixpkgs.hello.x86_64-linux` |
| `nixos-YY.MM-small`                          | `nixos/release-YY.MM-small` | `nixpkgs.hello.x86_64-linux` |

A channel of another system, like `nixpkgs-26.05-darwin`, has no row.

**The build history.** Each row shows the last 30 finished builds of the job as a strip, the oldest on the left. Green passed, red failed, grey was aborted or cancelled. Hover a cell for its status and time. The strip links to the job on Hydra. "N of M passed" counts the builds that were not aborted or cancelled. The JSON API gives `hydra_channels`: for each channel, `channel`, `jobset`, `job`, `state`, `build` (with `history`, the newest first), `passed`, `counted` and `dead_job`.

**A job that Hydra no longer builds.** The page uses the attribute name as the job name. For an alias, Hydra may build the package under another name: `python3Packages.tornado` is built as `python314Packages.tornado`, and the job `python3Packages.tornado` last built in 2016. A job is dead when its newest build is more than 180 days old and no build in its history has the version that nixpkgs has now. Then the row says that Hydra stopped building the job, and shows no state and no strip. nixpkgs rebuilds nearly every package every few weeks, so a live job always has a recent build.

**The binary cache.** The output path of a package does not depend on its attribute name, so it works for an alias too. When the server reads a channel, it also computes the output path of each package that someone maintains or watches, in that channel's nixpkgs. One evaluation for a few hundred packages takes seconds; it does not evaluate the other packages. Then it asks `https://cache.nixos.org/<hash>.narinfo` with a `HEAD` request, at most 50 a minute. A path that is there is not asked again. A missing path is asked again after an hour. The "Binary cache" row shows, for each channel, "built", "not in the cache" or "not asked yet". Hover a word for the path. The JSON API gives `binary_cache`: for each channel, `channel`, `rev`, `out_path` and `cached`. There is no row for master: the index reads a master revision that Hydra has often not built yet. There is no row for a package that nobody maintains or watches, and a newly watched package gets its path when its channel moves to a new revision. "Not in the cache" does not tell why: the build failed, a dependency failed, or Hydra did not build it.

### The build watch

The server also follows the x86_64-linux builds of each package that someone maintains or watches, on master and on each channel of the table above. "Someone" is a person who signed in, saved a setting or watches a package. Two sources keep the state, and both are small for hydra.nixos.org:

- **The feed.** Every 5 minutes, the server asks `/api/latestbuilds` for the last 200 finished builds of `nixpkgs/unstable` on x86_64-linux, about 50 KB. When none of the 200 builds were seen before, it asks once more for 1,000.
- **The sweep.** One job at a time, at most 10 requests a minute, the server asks for the last 3 builds of each tracked package in each jobset. It asks for a package only when no build of it in that jobset was seen for 24 hours.

The feed sorts the builds by id, not by finish time. So a late build of a mass rebuild can fall out of the feed. The sweep finds it within a day. A build that a page view reads also counts. The watch ignores a build queued more than 180 days ago: when only such builds remain, it forgets the state of the package in that jobset.

The feed is for master only. Without a job, `latestbuilds` can be very slow on some jobsets: on `nixos/trunk-combined`, one request took 642 seconds on 2026-10-23. Nothing tells in advance which jobsets are slow like that. So the channel jobsets get the sweep only, which always names a job.

The state of a package is one of these:

| Hydra `buildstatus`          | State              |
| ---------------------------- | ------------------ |
| 0                            | passes             |
| 2                            | a dependency fails |
| 3, 4, 9 (aborted, cancelled) | no change          |
| any other                    | fails              |

The first build that the server sees only sets the state. A newer build with another state is a change. A change becomes an event of the kind "Hydra builds" in the inbox and the Atom feed of each person who maintains or watches the package: "Hydra build fails", "Hydra build fails: a dependency fails" or "Hydra build fixed". For a channel, the title ends with its name, such as "Hydra build fails on nixos-26.05". The event links to the build on Hydra. The server keeps the changes for 45 days. [Needs you](personal.md#needs-you) and the flags of [your packages](personal.md#your-packages) read the stored state. `/state?tab=status` and `hydra_builds` in `/api/v1/sync` show the counts. Requests to Hydra name Maintainer Mode in the `User-Agent`.

## Search

Each page has a search box in its header. Press `/` or `s` on any page to
go to it. On a phone, the box is an icon that opens `/search`.

From 2 characters, the box shows up to 5 suggestions of each kind, and
"All results". The up and down arrows, or Ctrl+N and Ctrl+P, select one.
Enter opens it. Esc closes the list, then leaves the box.

A query that names one page opens that page at once:

| You type                                  | It opens           |
| ----------------------------------------- | ------------------ |
| an attribute or a package name            | `/p/<attr>`        |
| `nixos/<name>` or a module file           | `/m/<module file>` |
| a nixpkgs path                            | `/a/<path>`        |
| `@login`                                  | `/u/<login>`       |
| `#123`, `123` or a link to a pull request | `/pr/123`          |

Other text opens `/search?q=<text>`. It lists, in this order:

- packages, NixOS modules and busy directories whose name holds the text;
- maintainers whose login holds the text;
- synced pull requests whose title has the text at the start of a word (3 characters or more),
  open ones first;
- advisories with that id: a tracker code (`NIXPKGS-…`), a CVE or an OSV id
  (`GHSA-…`), with the packages that they affect. Words never match an
  advisory.

`/api/v1/search` and the MCP tool `search` give the same lists.

## Keyboard shortcuts

The keys follow GitHub. Press `?` on any page to see the keys of that page.
The keys do nothing while you type in a field.

| Key            | What it does                                      |
| -------------- | ------------------------------------------------- |
| `/` or `s`     | go to the search box                              |
| `g h`          | go to Home                                        |
| `g r`          | go to Review                                      |
| `g p`          | go to Packages                                    |
| `g s`          | go to nixpkgs (`/state`)                          |
| `g n` or `g i` | go to the inbox                                   |
| `j` / `k`      | select the next row / the row before, in any list |
| `o` or Enter   | open the selected row                             |
| Esc            | close the list of keys or clear the selection     |
| `?`            | show the keys of the page                         |

Some pages add keys, such as `n` and `e` of
[Needs you](personal.md#keys). The first Tab of a page goes to "Skip to
content".

## The risk score

Each package gets one number: how much it needs attention. The number is a
sum, and each part shows its reason. The package page lists the reasons.
On a list, the label shows them when you point at it. Lists show the
highest number first: the packages of an area, what you maintain, what you
watch, your package set, and the same lists in the JSON API (`risk`,
`risk_band`, `risk_reasons` in `flags`).

| Cause                                                    | Points                                                         |
| -------------------------------------------------------- | -------------------------------------------------------------- |
| Each open advisory that affects master, by its worst CVE | critical 40, high 25, medium 10, low 3, no score 5; 60 at most |
| Each OSV match for the master version                    | the same points, in the same 60                                |
| Each open advisory with an unknown range                 | the same points, in the same 60                                |
| Each advisory fixed in master that a channel still has   | half the points, rounded up, in the same 60                    |
| Each advisory fixed in master and in every channel       | 0                                                              |
| One advisory exploited in the wild (CISA), not fixed     | 20, or 8 for a public exploit                                  |
| The highest EPSS of an advisory that is not fixed        | 10 from 10 %, 4 from 1 %                                       |
| `meta.knownVulnerabilities`                              | 15                                                             |
| `meta.broken`                                            | 5                                                              |
| Behind upstream                                          | 12 for a new major version, 8 otherwise                        |
| No maintainer and no team                                | 6                                                              |
| Packaging hygiene gaps                                   | 1 each, 3 at most                                              |

The bands are: 0 ok, 1 to 14 low, 15 to 29 medium, 30 to 49 high, 50 and
more critical. The weights are a policy, not a measurement. The score does
not judge pull requests and does not read the git log.

The reasons name the verdict: "1 open advisory affects 1.26.0", "1 open
advisory, range unknown" or "1 advisory fixed in master, still in
nixos-26.05".

## Affected versions

The tracker keeps an advisory open until someone closes its nixpkgs issue.
Often nixpkgs already ships the fix. Maintainer Mode compares the affected
ranges with the versions that nixpkgs ships. It does not trust a range that
it cannot read: in doubt, the verdict is "range unknown", and the advisory
counts as open.

- **Ranges.** The tracker copies the affected versions of each CVE record.
  Maintainer Mode keeps them for each advisory and each attribute. It reads
  these forms: `<1.0`, `=<1.0`, `==1.0`, `==< 1.0`, `==<= 1.0`, `=== 1.0`,
  `==>= 1.0, < 1.2`, `==1.0 to 1.2` and `*`, as `affected`, `unaffected` or
  `unknown`. A version may start with `v`.
- **Which product.** A CVE record names products. A range counts for an
  attribute only when the product name is the name of the attribute, or that
  name with a major version or a build variant (`ffmpeg_9-headless`,
  `unbound-with-systemd`). The range of `immich` does not judge
  `gnomeExtensions.immich-wallpaper`.
- **The title.** When the ranges of a CVE do not decide, the title of the
  advisory can: `unbound: security issues < 1.26.1`, `Flower before 2.2.0`,
  `foo through 3.1`. The title counts only when a word before the version
  names the attribute.
- **What stays unknown.** A version that is not a release (a commit hash,
  `unstable`, a Go pseudo-version, a calendar version against a bound of
  another scheme). A list of exact affected versions that does not name the
  version: such a list often names only the version that someone tested. A
  title that names an affected version which the checked version has not
  passed: then the ranges are wrong.
- **Order.** Versions compare as in [Upstream versions](#upstream-versions):
  1.10 is newer than 1.9, and 1.0-rc1 is older than 1.0.
- **The verdict.** One affected CVE makes the advisory affect a version.
  Maintainer Mode checks the master version first, then the version in each
  [channel](#channels):
  - **affects master**: master has an affected version;
  - **fixed in master**: master is fixed, and a channel still has an
    affected version. A stable channel needs a backport;
  - **fixed everywhere**: master and every channel read are fixed. The
    tracker issue can be closed. With no channel read yet, master decides
    alone;
  - **range unknown**: the data does not tell.
- **Counts.** "With open advisories" on `/state`, on the home page and in
  the lists counts only advisories that affect master or have an unknown
  range. `/state` also says how many packages have only advisories fixed in
  master, and lists the tracker issues to close under "Tracker issues to
  close". `GET /api/v1/metrics` gives the list under
  `packages.tracker_issues_to_close`, with the attribute, the advisory, the
  issue, the fix and the master version. `GET /api/v1/packages/<name>` gives
  each advisory its `verdict`. Maintainer Mode never writes to GitHub: a
  person closes the issue.
- **Needs you.** A machine report keeps the version of each package when the
  client sends one. The client sends a version only when nixpkgs has that
  exact version. When the ranges tell, that version decides: an affected
  version makes an item, a version that is not affected makes none. Without
  a version, or when the ranges do not tell, the verdict decides. "Fixed
  everywhere" makes no item. "Fixed in master" still makes an item, because
  the machine can run a channel.
- **Pull requests.** The Security block of `/pr/<n>` shows the verdict of
  each advisory on the packages that the pull request changes. Advisories
  fixed everywhere go to an "Already fixed" line, and they alone do not show
  the block.

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
  are behind. `GET /api/v1/packages/<name>` gives the same under `channels`.
- **Needs backport.** An advisory whose nixpkgs issue closed in the synced
  window counts as fixed in master. When a stable channel has an older
  version than master, that advisory shows "needs backport". It stays on the
  page while a stable channel is behind. An open advisory whose verdict is
  "fixed in master" gets the hint too, for each stable channel with an
  affected version. The API lists both under `needs_backport`.
- **What I maintain** marks each package that a channel has at an older
  version: "behind in …". The API gives `channels_behind`.
- **`/state?tab=status`** has one line per channel, with its revision and when it was
  first seen.
