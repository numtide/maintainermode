# Maintainer Mode

**What should a nixpkgs maintainer do next, for what we give back?**

I ([zimbatm](https://github.com/zimbatm)) build Maintainer Mode for myself, to
make my maintenance of nixpkgs more enjoyable. It puts in one place what I would
otherwise collect by hand from GitHub, Hydra, repology and the security tracker.
It runs in public, so other maintainers can use it too, but it follows one
person's way of working. It is not an official NixOS project.

The pages answer the question: **pull requests to review**, **pull requests
ready to merge**, a page for **each package, NixOS module and area** with its
health, issues and pull requests, **what I maintain**, and **what I watch**. A
further page is your **review record**.

Review someone else's nixpkgs pull request, and yours gets on the list.

Reviews are cheap and merges are scarce. An approval from someone without
commit access moves nothing, so a club that only trades reviews builds a nicer
waiting room. The product here is the **ready-to-merge queue**: pull requests
that carry a review that changed something, pass their checks, have no
conflict, and have an approval on their current head. A committer can work it
from the top down. The review queue exists to feed it.

Maintainer Mode reads GitHub. **It never writes to nixpkgs**: no reviews, no
merges, no comments, no labels. The one write it makes is on your click: it
starts a workflow in a repository you own (see [Actions](#actions)), and that
workflow acts as you.

## Maintainer Mode grants nothing

nixpkgs committers grant commit access. Maintainer Mode hands them evidence
they do not have today, and a human still decides.

Getting a pull request listed is a cheap prize, which is why additive-only
credit and no dispute process are enough. Commit access is an expensive
prize: the moment a number buys it, the incentive to game it outgrows what
this design can hold. So nothing here computes a recommendation, and no
number unlocks anything beyond a place in the queues.

The record answers: here are this person's last fifty reviews, what came of
each, and whose work they overlap with. Its headline is **pull requests a
committer later merged without further changes after their review**. Nothing
else shows as directly that someone's judgement already matches the people who
hold the merge button, and it cannot be farmed, because a committer has to act
for it to count. The second number is **problems found**: the author changed
the pull request after a review that requested changes or commented on a
changed line.

There is **no leaderboard**. A leaderboard drives volume, and volume is the
enemy of review quality. A global ranking lets five people dominate while the
rest disengage. The visible unit is the outcome on one person's own page, not
a count of reviews and not a rank.

## Within the nixpkgs rules

Maintainer Mode works inside the nixpkgs
[automation/AI policy](https://github.com/NixOS/nixpkgs/blob/cb05a63e8584e596c5747f368669e7c7ff3615aa/CONTRIBUTING.md#automationai-policy)
and does not add rules of its own on top of it.

- **A person merges.** The policy says automation without manual review "must
  not be used as the sole arbiter of whether to merge a change". The
  ready-to-merge queue is a reading list for a committer. It merges nothing.
- **A credited review is a contribution.** The policy covers reviews: a review
  made with an AI tool must be reviewed by its author and disclosed. Maintainer
  Mode credits reviews by their outcome, and the policy applies to them as it
  applies to any other review.
- **The AI numbers are aggregates.** The [state of nixpkgs](#the-state-of-nixpkgs)
  counts disclosures. It never lists a person or a pull request. The policy
  leaves enforcement to people, who are asked to "assume good faith and remain
  civil".

## The rules

### 1. Credit is earned by moving a pull request, not by having an opinion

A review earns credit when it is on someone else's pull request and at least
one of these follows within **14 days**:

| Trigger                  | The GitHub fact                                                                  |
| ------------------------ | -------------------------------------------------------------------------------- |
| The author pushed        | a commit on the head, or a force push, after the review                          |
| The author replied       | a comment by the author, or a reply in a review thread, after the review         |
| Inline on a changed line | the review carries an inline comment whose line is a `+` or `-` line of the diff |
| It merged                | the pull request merged after the review                                         |

Every trigger is a fact a program reads; nothing needs a judge. Three of the
four need the other party to act, so one person cannot farm them alone.

**What this rejects.** A bare "LGTM", an approval with no comment, an
approval on a pull request that then sits unmerged, a comment on an unchanged
context line, a review by a bot, and any review of your own pull request.

**Reviewing your own pull request never counts.**

### 2. Whose pull requests appear

A pull request appears in either queue when its author is **listed**: their
reviews earned credit on pull requests of at least **three different authors**
in the last **60 days**. A pair trading reviews cannot reach three. A ring big
enough to reach it looks like a healthy review community, and it is one.

The age limit is the window: a review older than 60 days no longer counts
towards being listed. Its credit also decays (below), so old help fades
rather than vanishes.

### 3. How the ledger is computed

The ledger is a pure function of public GitHub data and the clock
(`src/ledger.rs`). Nothing in it is entered by hand. Delete the database,
sync again, and you get the same ledger. Every credit links to the review that
earned it.

- **Additive only.** Credit is never taken away. Nobody is banned or
  demoted, so there is nothing to appeal and no dispute queue to staff. A
  drive-by reviewer does not accrue, and their pull request does not appear.
  That is the whole sanction.
- **Decay, not deletion.** Each credit halves every 30 days. The list shows
  who helps now. Decay is the same for everyone.
- **Caps.** One credit per reviewer per pull request. At most five credits
  per reviewer per UTC day; the excess shows on the ledger as "over the daily
  cap".
- **Per-pair diminishing returns.** Inside the window, your first credit on a
  given author counts 1, the second 1/2, the third 1/4. Two people cannot pump
  each other, and nobody has to decide whether they colluded.
- **Queue-head weight.** A review of a pull request that had waited 14 days
  or more earns full weight. A fresh one you picked yourself earns 1/2. A
  review of an **unowned** pull request (no `meta.maintainers` and no
  `ci/OWNERS` entry for any file it touches) earns full weight at any age.
  The hygiene review a newcomer can do there pays at least as much as a deep
  review in a busy area.
- **Reciprocity ratio.** Each profile shows the share of credit that came
  from authors who also earned credit on that person's pull requests. It is
  shown and never acted on.
- **The one manual lever.** A nixpkgs committer can void a review. They post
  a comment on the same pull request with a line
  `maintainer-mode: void <link to the review>`. The comment is public, so the act
  is logged. The ledger shows the voided credit with a link to that comment.
  "Committer" means the comment's `authorAssociation` is `OWNER`, `MEMBER` or
  `COLLABORATOR`.

### The constants, and why

| Constant                 | Value | Why                                                                                                                                                  |
| ------------------------ | ----- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| Move window (N)          | 14 d  | Long enough for a volunteer author to answer over two weekends. Longer, and an unrelated push or merge starts to buy credit.                         |
| Decay half-life          | 30 d  | One month: a person who stops reviewing drops to a quarter in two months. Shorter punishes a holiday; longer lets the list lag behind who helps now. |
| Window                   | 60 d  | Two half-lives: credit older than that is at 25% and no longer says much. It also bounds the first sync of nixpkgs to a size one token can pull.     |
| Distinct authors to list | 3     | The smallest number a pair cannot reach. Higher shuts out people who know a small corner of nixpkgs well.                                            |
| Daily cap                | 5     | A careful nixpkgs review takes 15 to 60 minutes. Five is a heavy but honest day; twenty in an hour is spraying.                                      |
| Per pull request         | 1     | More rounds on one change are the same piece of work. Without this, ten "nit" reviews on one pull request beat one useful review.                    |
| Pair factor              | 1/2   | Halving lets a real working pair still count for something, while the sum over any number of repeats stays below 2.                                  |
| Queue head age           | 14 d  | Most nixpkgs pull requests get their first attention, if ever, in the first two weeks. Past that they are the backlog the club exists to drain.      |
| Fresh-pick factor        | 1/2   | People keep the freedom to review what they understand, and it still pays; the backlog pays double.                                                  |

`GET /api/v1/status` returns the live values.

## Routing: shown by competence, paid by outcome

What a person is shown is filtered to their area. What earns credit is still
only whether the review moved the pull request, so a review outside your area
counts when it moves something; you are simply not asked for it.

Your area is derived, never self-assessed. It is the union of:

1. **`meta.maintainers`**: every package whose evaluated `meta.maintainers`
   names your GitHub login, at the directory of its `meta.position`. Teams and
   computed lists count, because the evaluator resolves them.
2. **`ci/OWNERS`**: the file the "Check / owners" job reads. Team entries are
   skipped, because team membership is not public.
3. **What you changed**: the directories of files in your merged pull requests
   over the last year, read with your own token when you log in.

On top of that you can add **tags**, path prefixes such as
`pkgs/development/python-modules`, for what you just started caring about.

A pull request that no member's area covers is **unowned**. It gets its own
bucket, which is the newcomer path: packaging hygiene (complete `meta`,
pinned hashes, `platforms`, a missing `versionCheckHook`) needs no domain
knowledge and is most of what a first review finds.

## Packages, modules and areas

Every package, NixOS module and nixpkgs path has a page: `/p/<attr>`,
`/m/<module file>` and `/a/<path>`. `/packages` searches them. Each page shows
health first, then the open issues and pull requests, then the ones closed or
merged in the synced window.

- **Health of a package:** its nixpkgs version and the newest upstream release
  (only when nixpkgs is behind; see [Upstream versions](#upstream-versions)), the last
  Hydra build of `nixpkgs/unstable/<attr>.x86_64-linux`, the open advisories
  of the [security tracker](https://tracker.security.nixos.org), and its
  maintainers and teams. An advisory counts as open while its nixpkgs issue is
  open. Each advisory shows the worst severity of its CVEs (CVSS), whether
  CISA saw an exploit, and its [EPSS](https://www.first.org/epss/) score: the
  chance of exploitation in the next 30 days. A package from PyPI,
  crates.io or npm also shows the [OSV](https://osv.dev) advisories for the
  exact version nixpkgs ships, unless a tracker advisory names the same CVE.
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

## What I maintain

`/u/<login>/maintains` lists the packages and NixOS modules whose
`meta.maintainers` names the login, or whose `meta.teams` names a team the
login is in. Each row carries the same health flags as a page. The highest
[risk score](#the-risk-score) comes first. The list comes from nixpkgs;
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

[`docs/rescan.md`](docs/rescan.md) has the threat model, the sandbox, the
limits, and why the feature is off by default.

## Actions

`/actions` starts work in GitHub Actions of a repository you own, with your
token, when you press a button. Nothing runs on the Maintainer Mode host.

- **Review in CI**, on any pull request row or by number, starts `review.yml`
  of your fork of
  [nixpkgs-review-gha](https://github.com/Defelo/nixpkgs-review-gha) with the
  pull request number. Its own defaults decide the rest, like posting the
  result.
- **Update with nix-update**, on a package page, starts `update.yml` with the
  attribute and the version (repology's newest release, which you can change).
  It is a small re-implementation of nixpkgs-update on
  [nix-update](https://github.com/Mic92/nix-update): clone nixpkgs, run
  nix-update (the package's `updateScript` when it has one and no version is
  given), build the package, and push `update/<attr>` to your nixpkgs fork.
  **The branch is pushed whether the build passes or not**, so a failed update
  is a branch to keep working on. It never opens a pull request: the run and
  `/actions` link the page where you open one. A second job pushes the branch.
  Only that job gets `GH_TOKEN`, and it runs no code from nixpkgs or upstream.
- `/actions` lists your recent runs of both workflows, and the branch of each
  update.

To set it up:

1. Fork nixpkgs-review-gha, and copy [`gha/update.yml`](gha/update.yml) to its
   `.github/workflows/`. It calls the reusable workflow in
   `.github/workflows/update.yml` of this repository.
2. Give it a `GH_TOKEN` secret that can push to your fork of nixpkgs.
3. Install the Maintainer Mode GitHub App on that repository. The App needs
   the "Actions: read and write" permission, so that your login token can
   start a workflow.
4. Check the repository names on `/actions`. They default to
   `<login>/nixpkgs-review-gha` and `<login>/nixpkgs`.

## LLMs, if you want them

`/me/settings` has one choice: **"I am OK with LLMs using my data on this
site"**. It is off by default. Each person sets it for themselves. When it is
off, nothing below is available to that person.

### The MCP server

`/mcp` is a read-only [MCP](https://modelcontextprotocol.io) server. It uses
JSON-RPC 2.0 over the Streamable HTTP transport of the MCP specification
2025-06-18.

- **The token.** Make it on `/me/settings`. The server shows it once and keeps
  only its SHA-256 hash. A new token replaces the old one. One button revokes
  it. Turning the choice off also revokes it.
- **A request** is a POST with one JSON-RPC message and
  `Authorization: Bearer <token>`. The answer is one JSON object. There is no
  session and no server-sent stream, so `GET /mcp` answers 405.
- **Methods:** `initialize`, `ping`, `tools/list` and `tools/call`.
- **Tools.** Each tool only reads. Each returns its JSON as text content. No
  tool writes to GitHub.

| Tool           | Returns                                                                       |
| -------------- | ----------------------------------------------------------------------------- |
| `search`       | packages, modules and areas that match a name, with their health flags        |
| `package`      | one package, as `/api/v1/packages/<attr>`                                     |
| `module`       | one NixOS module, as `/api/v1/modules/<file>`; `nixos/<name>` also works      |
| `area`         | one path of the tree, as `/api/v1/areas/<path>`                               |
| `maintains`    | what a login maintains, as `/api/v1/maintainers/<login>`                      |
| `queue_merge`  | the ready-to-merge queue, the first 50 or `limit`                             |
| `queue_review` | the review queue: `bucket` is `mine` (default, your area), `unowned` or `all` |
| `watching`     | your subscriptions, with their health flags                                   |
| `sync_status`  | the data of `/sync`                                                           |

| Code | Meaning                                                       |
| ---- | ------------------------------------------------------------- |
| 200  | a JSON-RPC answer                                             |
| 202  | a notification; there is no answer                            |
| 400  | an `MCP-Protocol-Version` other than 2025-06-18 or 2025-03-26 |
| 401  | no token, or an unknown token                                 |
| 403  | the choice is off, or an `Origin` other than this site        |

To add it to Claude Code on your machine:

```sh
claude mcp add --transport http maintainer-mode https://<host>/mcp --header "Authorization: Bearer <token>"
```

### A Claude Code session on a pull request

Each open pull request row gets a **Claude Code** button next to **Review in
CI**. It opens `/claude/pr/<number>`. That page shows the prompt in a copy
box. Its link opens [Claude Code on the web](https://claude.ai/code) with the
prompt filled in and `NixOS/nixpkgs` selected, through the documented
`prompt` and `repositories` parameters. Nothing runs until you send the
prompt. The session runs in your own Claude account.

The prompt gives:

- the pull request number, its URL, its base branch and the files it changes.
  The title and the file paths come from the author. The prompt puts them in a
  fenced block and marks that block as data, not instructions;
- the nixpkgs
  [automation/AI policy](https://github.com/NixOS/nixpkgs/blob/master/CONTRIBUTING.md#automationai-policy):
  you review and answer for the result, each commit ends with an
  `Assisted-by:` trailer that names the tool and the model with its version,
  and review comments and descriptions disclose AI use apart from the commits;
- a rule: do not push to nixpkgs, and do nothing on GitHub without your word;
- the `/mcp` address, when you have an MCP token. The token itself never goes
  into the prompt.

## The state of nixpkgs

`/state` and `GET /api/v1/metrics` show how pull requests flow through
nixpkgs, month by month. Every figure is an aggregate. No figure names a person,
in line with "no leaderboard".

- **Inflow.** Pull requests opened, and how many came from bots, first-time
  contributors, security fixes, new packages, backports and mass rebuilds.
  "First-time" is GitHub's `authorAssociation` or the nixpkgs label
  `12.first-time contribution`. The other kinds come from the nixpkgs labels
  (`1.severity: security`, `8.has: package (new)`, `10.rebuild-linux: 501+`)
  and from a `release-*` base branch.
- **Disclosed AI use.** A pull request counts when its last commit or its
  description has an `Assisted-by:` trailer, or when it has the
  `llm-assisted` label. AI attribution in another form, such as a
  `Co-authored-by:` trailer, is counted apart, because the policy says it does
  not satisfy the disclosure rule. Undisclosed use cannot be seen, so each count
  is a floor. Standard community automation, such as `r-ryantm`, is exempt
  from the policy and counts under bots.
- **Outflow.** Merged, closed without merge, the backlog open at each month's
  end, merges without an approval, and self-merges.
- **Latency.** Hours to the first review by someone else and the share
  reviewed within seven days, both for human authors. Days to merge, at the
  median and the 90th percentile.
- **Outcomes by cohort.** Of the pull requests opened in a month and closed
  since, the share merged: for all, for first-time contributors and for
  disclosed AI use.
- **Load.** Over the last 365 days, how many people did half, and 80%, of the
  merges, of the reviews and of the human-authored pull requests. This shows how
  few people carry nixpkgs without naming them.
- **Areas.** Open, never reviewed, opened and merged per area of the tree, from
  the files a pull request changes.

The full sync reads only pull requests updated in the last 60 days and all open
ones. A second walk, the **history walk**, reads every pull request created
since `--history-since` (`MAINTAINER_MODE_HISTORY_SINCE`, default 2020-01-01).
It reads 50 pull requests per query with fewer fields: the first 10 reviews,
the first 50 files, the labels and the last commit message, but not the
description. It stores them in a table of their own, so
they never reach the ledger. It walks 100 pages per sync pass and resumes from
its cursor. The first walk back to 2020 takes several hours. Until it ends,
older months are incomplete. The backlog before `--history-since` is a floor,
because a pull request closed before the walk started is not in it.

## Four audiences

| Audience | Surface                                                                                                                                                                                                                                                                                                                                                              |
| -------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Humans   | the web page: ready to merge, needs review (your area, unowned, all), a page per package, module and area, what you maintain, what you watch, your package set, actions, one review record per person                                                                                                                                                                |
| Agents   | `GET /api/v1/queue/merge`, `/api/v1/queue/review?bucket=…&login=…`, `/api/v1/people/<login>` (record, credits, area), `/api/v1/find/<name>`, `/api/v1/packages/<attr>`, `/api/v1/modules/<file>`, `/api/v1/areas/<path>`, `/api/v1/maintainers/<login>`, `/api/v1/metrics`, `/api/v1/status`, `/api/v1/events` (Server-Sent Events), and `/mcp` for those who opt in |
| Programs | `maintainer-mode check <login>`: exit 0 listed, 3 not listed, 1 broken; `show <name>` and `maintains <login>`                                                                                                                                                                                                                                                        |
| Owners   | `maintainer-mode scan`, `export` and `submit`: a private package set from your own closure                                                                                                                                                                                                                                                                           |

```sh
maintainer-mode --server https://<host> merge
maintainer-mode --server https://<host> review --bucket mine --login <you>
maintainer-mode --server https://<host> record <login>
maintainer-mode --server https://<host> check <login>; echo $?
maintainer-mode --server https://<host> metrics --months 24
```

Every query command takes `--json`.

## How the data is pulled

- One GraphQL query reads 25 pull requests with their reviews, review
  comments, author comments, pushes, files and check status. There is no
  request per pull request.
- The first pass reads every open pull request. After that, each pass walks
  pull requests by `updatedAt`, newest first, and stops at the watermark of
  the previous pass. Both walks store their cursor, so a pass that runs out
  of budget resumes where it stopped.
- **Issues** are read the same way, 100 to a query. The body is read but not
  kept: only the paths and the package name it gives are. The first pass reads
  every open issue, then each pass walks by `updatedAt`.
- **Tokens.** When you log in, Maintainer Mode keeps your token and reads public
  nixpkgs data with it. The rate limit grows with the number of people who use
  it. The login asks for no scope. The token reads public data, and it can
  start workflows only in the repositories where you install the Maintainer
  Mode GitHub App (see [Actions](#actions)). The sync uses it to read only. A
  deployment may also give the server a token of its own
  (`MAINTAINER_MODE_SYNC_TOKEN_FILE`), which is used first. Signing out deletes
  your session and its token.
- **A GitHub App.** A deployment can sync with the token of a GitHub App
  installation (`GITHUB_APP_ID`, `GITHUB_APP_KEY_FILE`). The service signs a
  JWT with the App's private key and exchanges it for an installation token.
  That token expires after one hour, so the service gets a new one before
  each pass that starts in its last 30 minutes. The App token is used first.
  Install the App on the account that owns it. People also install the App on
  their own repositories, so the service never takes their installations. Set
  `GITHUB_APP_INSTALLATION_ID` to use a different installation.
  When the App is set up, remove `MAINTAINER_MODE_SYNC_TOKEN_FILE`: the
  deployment then needs no token of a person. The token file stays as a
  fallback.
- **No network yet.** On first boot, a pass can start before the network is
  up. When the GitHub sync fails to connect, the pass stops and the next pass
  starts after 5 seconds. The wait doubles after each failed pass, up to 5
  minutes. The first pass that connects restores the normal interval. Only the
  GitHub sync counts: when another source is down, the pass continues.
- **The nixpkgs index is evaluated, with iets.** Once a day, Maintainer Mode
  unpacks the source tarball of nixpkgs `master` and evaluates every derivation
  in the top level and `python3Packages` (`--index-set`). For each one it keeps
  the package name, the file of `meta.position`, and the GitHub logins in
  `meta.maintainers` and `meta.teams`. That one sweep maps paths to packages and packages to
  people. It also reads the first URL of `src` and the facts for packaging
  hygiene. Each of these is evaluated on its own, so one that fails keeps the
  package. They add about 380 MB to the peak of the top level. The inputs of
  a package are read only when its install check runs, because reading all
  of them adds 500 MB more. One more evaluation, of NixOS with no configuration, reads
  `meta.maintainers` and `meta.teams` of every module, and `lib.teams` gives
  the members of each team. A team member counts as a maintainer.
  It evaluates a local tree, not a flake reference, so the evaluator
  needs no daemon and writes nothing to a store.
- **The evaluator is one setting**, `--evaluator` (`MAINTAINER_MODE_EVALUATOR`):
  anything that answers `nix eval --json --file`. The package sets it to
  [iets](https://github.com/numtide/iets); `package.nix` takes `evaluator` to
  swap it. Each sweep logs its evaluator and time, so the two can be compared.
  Measured on one nixpkgs revision, both producing the same index:

  | Evaluator  | Top level (25,320) | python3Packages (11,484) | Whole sweep | Peak RSS |
  | ---------- | ------------------ | ------------------------ | ----------- | -------- |
  | iets       | 7.1 s              | 1.8 s                    | 13.9 s      | 1.98 GB  |
  | nix 2.34.8 | 13.1 s             | 6.3 s                    | 19.6 s      | 2.07 GB  |

  `maintainer-mode sweep` runs the sweep alone, to time it.

- **Health** comes from two daily bulk reads, kept in the database:
  repology's list of projects outdated in `nix_unstable`, at one request a
  second, and every issue of the security tracker. Hydra is asked per page
  view and the answer is kept an hour. The health sources run in their own
  task, every 10 minutes. A slow source does not delay the pull request and
  issue sync.
- **Advisory detail.** For each CVE that the tracker names:
  - The CVSS score and the CISA exploitation decision come from the
    [CVE Program's records](https://github.com/CVEProject/cvelistV5), one
    file per CVE. A pass reads up to 300. A record is read again after 7 days
    while it has no score, and after 90 days when it has one.
  - [NVD](https://nvd.nist.gov) gives a score to a CVE whose record has
    none. NVD needs an API key for a useful rate, so it is off without
    `MAINTAINER_MODE_NVD_API_KEY_FILE`. With a key, a pass asks for up to 50.
    While the key is set, every page says, as the NVD terms of use ask:
    "This product uses the NVD API but is not endorsed or certified by the
    NVD." A CVSS label says whether its score comes from NVD or from the CVE
    record. The risk score is Maintainer Mode's own, and its label says so.
  - [EPSS](https://www.first.org/epss/) scores every tracked CVE once a day,
    100 CVEs to a request.
  - [OSV](https://osv.dev) is asked once a day for each package whose
    ecosystem is known: a source from PyPI, crates.io or npm, or an
    attribute of a Python package set. One request asks for 1000 packages,
    each by its name and the exact version nixpkgs ships. A pass then reads
    the records of up to 200 new matches. When a record fails, the pass
    goes on with the others, and that record waits 1 day, then 2, 4 and up
    to 32 days. The CVEs of the matches get a score and EPSS too.

  No source needs an account. `/sync` shows what each source did on its last
  pass.

## The nixpkgs event log

Maintainer Mode keeps every public event on NixOS/nixpkgs that it can see.
[docs/events.md](docs/events.md) gives the plan and the measured size.

- **Sources.** With a sync token, the server polls the Events API at the
  interval GitHub asks for (`X-Poll-Interval`). It sends the last `ETag`, so
  an answer of `304` costs no rate limit. Each hour, it also reads the
  GH Archive file of the past hour and adds the events that the poll missed.
  Without a sync token, GH Archive is the only source. GH Archive holds only
  about 2 to 3 % of the nixpkgs events.
- **Storage.** One SQLite file per month: `events/YYYY-MM.sqlite`, next to
  the database. Each event has a `seq` that continues across the files. The
  GitHub event id is unique, so an event that both sources send is stored
  once. The event JSON is zstd-compressed.
- **Reading.** `GET /api/v1/events` is Server-Sent Events. Each event has its
  `seq` as `id` and the event JSON as `data`. Send `Last-Event-ID` (or
  `?after=<seq>`) to replay from that point, then follow live. Without it,
  the stream starts at the newest event. Filters: `type`, `number`, `actor`.

```sh
curl -N -H 'Last-Event-ID: 0' 'https://<host>/api/v1/events?type=PullRequestReviewEvent'
```

- **Copy outside the host.** This is off by default. When you set the four
  `MAINTAINER_MODE_S3_*` variables, the server copies to an S3-compatible
  store (the plan uses bunny.net Storage with its S3 API):
  - each day, after 02:00 UTC: the events that GitHub created on the day
    before, as `YYYY/MM/DD.ndjson.zst`, in the GH Archive format;
  - each past month, once: its SQLite file, as `YYYY/MM.sqlite.zst`.

  The credentials file holds two lines, `AWS_ACCESS_KEY_ID=…` and
  `AWS_SECRET_ACCESS_KEY=…`. The NixOS module reads it through
  `LoadCredential` (`services.maintainer-mode.eventCopy`).

## The archive of issues and pull requests

Maintainer Mode keeps a full copy of each issue and pull request of
NixOS/nixpkgs, so that GitHub is not the only place that holds them.
[docs/ingest.md](docs/ingest.md) gives the plan, the measured size and the
cost.

- **What.** The raw GraphQL JSON of each item: title, body, author, labels,
  state, timestamps, all comments, all reviews with their bodies, all inline
  review comments (with path, line and diff hunk), the files and the
  timeline.
- **Storage.** One SQLite file per repository: `archive/<owner>/<name>.sqlite`,
  next to the database. The JSON is zstd-compressed. The main database does
  not change.
- **The walk.** A background task, apart from the sync, starts a pass every
  5 minutes. Each pass fetches a fixed number of items per repository. It
  first fetches what changed recently, then walks back from the newest
  number to #1. An item that gets an event in the event log is fetched again
  5 minutes later. The walk keeps its cursors in the archive file, so it
  continues after a restart. It uses only the GitHub App token and the sync
  token of the server, never the tokens that people lend. It stops for the
  pass when the token has less than 1000 points left.
- **Reading.** `GET /api/v1/archive/<owner>/<repo>/<number>` gives the stored
  JSON, or 404. `/sync` shows the progress. The endpoint has no
  authentication, so the archive keeps public repositories only: it refuses
  a repository that GitHub reports as private.

```sh
curl https://<host>/api/v1/archive/NixOS/nixpkgs/1
maintainer-mode archive export --repo NixOS/nixpkgs --out ./export
```

The export writes one `ndjson.zst` file per 10 000 numbers. To archive more
repositories, add them to `services.maintainer-mode.archive.repositories`.
An empty list turns the archive off.

## Known limits of slice one

- Each pull request reads its first 30 reviews, the first 5 inline comments of
  each, the last 30 comments, the last 30 pushes and the first 50 files. A very
  busy pull request can lose a trigger or a void past those limits.
- Ownership of a pull request is judged against today's nixpkgs, not the
  nixpkgs of the day of the review.
- The sweep covers derivations in the swept sets, and NixOS modules. A package
  outside the swept sets has no page.
- Linking issues and pull requests to a package reads conventions, not
  intent. A title that names no package and touches no file of it is missed.
- Bot authors (`r-ryantm` among them) are never listed, so their pull requests
  do not appear.

## Run it

```sh
nix build
GITHUB_TOKEN=… ./result/bin/maintainer-mode sync --db club.sqlite --index
./result/bin/maintainer-mode serve --db club.sqlite
```

| Variable                               | Meaning                                                     |
| -------------------------------------- | ----------------------------------------------------------- |
| `MAINTAINER_MODE_LISTEN`               | listen address, default `127.0.0.1:8790`                    |
| `MAINTAINER_MODE_DB`                   | SQLite path                                                 |
| `MAINTAINER_MODE_PUBLIC_URL`           | the public URL; the OAuth callback is `<url>/auth/callback` |
| `GITHUB_CLIENT_ID`                     | OAuth app client id                                         |
| `GITHUB_CLIENT_SECRET_FILE`            | file holding the OAuth app client secret                    |
| `MAINTAINER_MODE_SYNC_TOKEN_FILE`      | optional token owned by the deployment                      |
| `GITHUB_APP_ID`                        | GitHub App whose installation token syncs first             |
| `GITHUB_APP_KEY_FILE`                  | file holding the PEM private key of the GitHub App          |
| `GITHUB_APP_INSTALLATION_ID`           | installation to use; else the one on the account of the App |
| `MAINTAINER_MODE_UPLOAD_TOKEN`         | the client's upload token for `submit`                      |
| `MAINTAINER_MODE_CHANNELS`             | channels next to master, default `nixos-unstable,stable`    |
| `MAINTAINER_MODE_S3_ENDPOINT`          | S3-compatible endpoint for the copy of the event log        |
| `MAINTAINER_MODE_S3_BUCKET`            | the bucket of that copy                                     |
| `MAINTAINER_MODE_S3_REGION`            | the region that the request signature names                 |
| `MAINTAINER_MODE_S3_CREDENTIALS_FILE`  | file with the access key id and the secret key              |
| `MAINTAINER_MODE_ARCHIVE_REPOS`        | repositories to archive; default `NixOS/nixpkgs`            |
| `MAINTAINER_MODE_ARCHIVE_ITEMS`        | items the archive walk fetches per repository and pass      |
| `MAINTAINER_MODE_RESCAN`               | `true` turns on the server rescans of public flakes         |
| `MAINTAINER_MODE_RESCAN_BWRAP`         | the `bwrap` program, default `bwrap`                        |
| `MAINTAINER_MODE_RESCAN_PRLIMIT`       | the `prlimit` program, default `prlimit`                    |
| `MAINTAINER_MODE_RESCAN_NIX`           | plain `nix` in `/nix/store`, default `nix` from `PATH`      |
| `MAINTAINER_MODE_RESCAN_MAX_FLAKES`    | flakes a person can register, default 3                     |
| `MAINTAINER_MODE_RESCAN_PER_DAY`       | scans a person gets in 24 hours, default 4                  |
| `MAINTAINER_MODE_RESCAN_TIMEOUT`       | wall-clock limit of one scan in seconds, default 900        |
| `MAINTAINER_MODE_RESCAN_MEMORY_MB`     | address-space limit of each sandboxed process, default 3072 |
| `MAINTAINER_MODE_RESCAN_PER_DAY_TOTAL` | scans in 24 hours for all people together, default 100      |

CI is the flake's `checks`: `nix flake check`.

## What we took, and under which licence

| Package                                                              | Version | Licence | Used for                                                                                            |
| -------------------------------------------------------------------- | ------- | ------- | --------------------------------------------------------------------------------------------------- |
| [`@primer/css`](https://www.npmjs.com/package/@primer/css)           | 22.3.2  | MIT     | `dist/primer.css`, served as-is                                                                     |
| [`@primer/octicons`](https://www.npmjs.com/package/@primer/octicons) | 19.40.0 | MIT     | thirteen 16px SVG icons                                                                             |
| nixfresh `nix.rs` and `attrs.rs`                                     | 2026-10 | MIT     | `src/eval.rs` and `src/attrs.rs`: the evaluator wrapper and the attribute index, copied and trimmed |
| nixfresh repology and tracker collectors                             | 2026-10 | MIT     | `src/health.rs`: the bulk reads of repology and the security tracker, rewritten for one host        |
| nixfresh `upstream.rs`, `version.rs` and direct collector            | 2026-10 | MIT     | `src/upstream.rs` and `src/version.rs`: upstream releases from tags and registries                  |
| nixfresh `score.rs`                                                  | 2026-10 | MIT     | `src/score.rs`: the risk score, simplified                                                          |
| nixfresh osv collector                                               | 2026-10 | MIT     | `src/osv.rs`: OSV matches for the exact version of PyPI, crates.io and npm packages                 |
| nixfresh cve, nvd and epss collectors                                | 2026-10 | MIT     | `src/severity.rs`: CVSS, CISA exploitation and EPSS for the tracker's CVEs                          |
| nixfresh `nix.rs` and `report.rs`                                    | 2026-10 | MIT     | `src/scan.rs` and `src/report.rs`: the closure scan, the report format and its redaction, trimmed   |

Both are fixed-output fetches of the npm tarballs, pinned by their sha512
(`nix/primer.nix`), and their licence files ship with the assets. The pages
are server-rendered HTML; there is no JavaScript.

Version 22 of `@primer/css` no longer ships `Box`, `Label`, `UnderlineNav`,
`Subhead` or `flash`. `src/club.css` rebuilds those few on Primer's own
color tokens, so light and dark mode follow the bundle.

The hues are the NixOS brand palette from
[NixOS/branding](https://github.com/NixOS/branding) (`colors.toml`, OKLCH):
Afghani Blue for links, buttons and the header, Argentinian Blue for focus and
the current tab, and the accent colors for pass, pending, fail and done.
`src/club.css` sets them on Primer's tokens, and Primer keeps its grays. The
site uses the colors only, not the NixOS logo, and it is not an official
NixOS project.

Primer is GitHub's design system, used so that people who arrive from a pull
request page do not learn a second interface. The club is an independent
community project. It is not a GitHub product and is not endorsed by GitHub,
and it does not use GitHub's name or logo as its own.

## Licence

MIT
