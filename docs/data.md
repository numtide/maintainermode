# Data

How Maintainer Mode reads GitHub and the other sources, what it keeps, and
its known limits.

## How the data is pulled

- **One fetcher.** The archive walk is the only reader of issues and pull
  requests (see [the archive](#the-archive-of-issues-and-pull-requests)).
  Each item that it stores in the NixOS/nixpkgs archive writes its rows of
  the main database: `pr` and `issue` for the open items and the items
  updated in the last 60 days, and `history` for every pull request, with
  `review_by` naming who reviewed each one. The
  rows have the caps of the projection (30 reviews, 5
  inline comments per review, 30 comments, 30 pushes, 50 files). A row with
  a newer `updatedAt` stays. After each full pass, the server deletes the
  closed rows of `pr` and `issue` last updated more than 60 days ago. When
  rows change, the pages are computed again,
  at most once every 30 seconds. Without an archive of NixOS/nixpkgs, the
  server has no issues and no pull requests.
- **The first start.** When the main database has no key
  `archive_projected`, or its key `archive_rows` is not the version of the
  rules for the rows, the walk first writes the rows of every item in the
  archive, 1000 items at a time, then sets the keys. It first copies the
  `mergeable`, CI state and author association of the open `pr` rows into
  `pr_live`, so the pages keep them, and it deletes the state of the old
  sync and history walks. `/api/v1/sync` gives the time as
  `rows_from_archive_at`.
- **Mergeable, checks and author association** change with no change to
  `updatedAt`. The table `pr_live` keeps them for each open pull request,
  with the head they belong to and the time of the check. The upkeep task
  runs every 10 minutes: it refreshes the nixpkgs index once a day, deletes
  expired data, and reads
  them for up to 500 open pull requests: first the merge candidates, then
  the ones checked longest ago, 50 to a query. GitHub computes `mergeable`
  lazily, so a bulk read mostly answers UNKNOWN; an UNKNOWN keeps the known
  value of the same head. These reads use the GitHub App token and the bot
  token. With neither, the lent tokens read the merge candidates only, with
  the limit below. `/state?tab=status` shows how many open pull requests
  have a check, and the oldest one; `/api/v1/sync` gives the same data as
  `live`.
- **Issues.** The body is read but not kept in the `issue` row: only the
  paths and the package name it gives are.
- **Tokens.** When you log in, Maintainer Mode keeps your token. It pays for
  one read that benefits you: the paths you changed in nixpkgs, when you
  sign in and then once a day. The login asks for no scope, so the token
  reads public data only. Signing out deletes your session and its token. A
  session ends after 30 days, as its cookie does, and the server then
  deletes it with its token.
- **Lent tokens and shared work.** The archive walk and the live refresh
  use the GitHub App token and the bot token. When these have less than
  1000 points left, or the server has neither, the lent tokens pay for the
  listings and the due items of the walk, and for the live fields of the
  merge candidates. Each lent token pays at most a tenth of its budget in
  one rate limit window for this shared work. The walk back through history
  never uses a lent token. `/me/settings#github` states this rule to each
  person who signed in. A server with no App token, no bot token and no
  lent token reads nothing from GitHub.
- **The bot token.** A deployment can give the server the classic token of
  its bot account (`MAINTAINER_MODE_BOT_TOKEN_FILE`). The archive walk uses
  it after the GitHub App token. The same token reads the notifications of
  the bot account (see [the notifications poller](#the-notifications-poller))
  and polls the Events API.
- **A GitHub App.** A deployment can read GitHub with the token of a GitHub
  App installation (`GITHUB_APP_ID`, `GITHUB_APP_KEY_FILE`). The service
  signs a JWT with the App's private key and exchanges it for an
  installation token. That token expires after one hour, so the service gets
  a new one before each pass that starts in its last 30 minutes. Install the
  App on the account that owns it. People also install the App on their own
  repositories, so the service never takes their installations. Set
  `GITHUB_APP_INSTALLATION_ID` to use a different installation. The App
  token comes before the bot token.
- **No network yet.** On first boot, a pass can start before the network is
  up. When GitHub cannot be reached, the pass stops and the next pass starts
  after 5 seconds. The wait doubles after each failed pass, up to 5 minutes.
  The first pass that connects restores the normal interval. Only GitHub
  counts: when another source is down, the pass continues.
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
  view and the answer is kept an hour. The
  [build watch](health.md#the-build-watch) has its own task: a feed of
  finished builds every 5 minutes, and a sweep of at most 10 jobs a minute.
  The health sources run in their own
  task, every 10 minutes. A slow source does not delay the archive walk.
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

  No source needs an account. `/state?tab=status` shows what each source did on its last
  pass.

## The notifications poller

The poller makes a change on GitHub show here in about a minute. The
listing of each archive pass stays as the safety net.

- **The bot account.** `maintainermode-bot` is a GitHub account for the
  machine, not for a person. It watches NixOS/nixpkgs with "All activity".
  Thus GitHub opens a notification thread for each issue and pull request
  that changes.
- **The token.** The bot token is a classic token of this account with one
  scope: `notifications`. Give it in `MAINTAINER_MODE_BOT_TOKEN_FILE` (the
  NixOS option `services.maintainer-mode.botTokenFile`). The server reads the
  file again when it changes, so a new token needs no restart. Without the
  token, the poller is off.
- **What it reads.** The poller reads the unread notifications of the bot
  account. It sends `If-Modified-Since` with the last `Last-Modified`, so an
  answer of `304` costs no rate limit. It waits the interval that GitHub
  gives in `X-Poll-Interval`, usually 60 seconds. It keeps only the threads
  of NixOS/nixpkgs whose subject is a pull request or an issue. The number
  comes from the subject URL.
- **What it does with a thread.** The poller marks that pull request or
  issue due in the archive and wakes the archive walk. The walk reads it
  about 10 seconds later, writes its rows and recomputes the pages and
  inboxes, at most once every 30 seconds. The poller reads nothing else
  from GitHub.
- **Read marks.** Then the poller marks the threads read, so the list stays
  short. If a mark fails, the threads stay unread, and the next poll tries
  again.
- **It never writes to nixpkgs.** The bot account makes no review, comment,
  label or merge. A read mark changes only the notifications of the bot
  account. It is not a write to nixpkgs.
- **Health.** The poller is healthy when its last good poll is at most
  three intervals old (at least 5 minutes). When it is not healthy, a
  change shows after the listing of the next archive pass, at most 5
  minutes later.
- **Status.** `/state?tab=status` shows whether the poller is on, its last
  poll, the threads of the last hour, the average time from a change on
  GitHub to its mark here, and the errors. `/api/v1/sync` gives the same data
  under `notifications`. The counts start again when the server restarts.

## The nixpkgs event log

Maintainer Mode keeps every public event on NixOS/nixpkgs that it can see.
[docs/events.md](events.md) gives the plan and the measured size.

- **Sources.** With a bot token, the server polls the Events API at the
  interval GitHub asks for (`X-Poll-Interval`). It sends the last `ETag`, so
  an answer of `304` costs no rate limit. Each hour, it also reads the
  GH Archive file of the past hour and adds the events that the poll missed.
  Without a bot token, GH Archive is the only source. GH Archive holds only
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
[docs/ingest.md](ingest.md) gives the plan, the measured size and the
cost.

- **What.** The raw GraphQL JSON of each item: title, body, author, labels,
  state, timestamps, all comments, all reviews with their bodies, all inline
  review comments (with path, line and diff hunk), the files and the
  timeline.
- **Storage.** One SQLite file per repository: `archive/<owner>/<name>.sqlite`,
  next to the database. The JSON is zstd-compressed. Each stored item of
  NixOS/nixpkgs also writes its rows of the main database (see
  [How the data is pulled](#how-the-data-is-pulled)).
- **The walk.** A background task starts a pass every 5 minutes. Each pass
  fetches a fixed number of items per repository. It first fetches every
  open item, once, and what changed recently, then walks back from the
  newest number to #1. An item that gets an event in the event log, or a
  notification of the bot, is fetched again about 10 seconds later. The
  walk keeps its cursors in the archive file, so it continues after a
  restart. It uses the GitHub App token and the bot token, and stops for the
  pass when they have less than 1000 points left. Then, and on a server
  with neither, the lent tokens pay for the listings and the due items
  only (see the tokens above).
- **Reading.** `GET /api/v1/archive/<owner>/<repo>/<number>` gives the stored
  JSON, or 404. `/state?tab=status` shows the progress. The endpoint has no
  authentication, so the archive keeps public repositories only: it refuses
  a repository that GitHub reports as private.

```sh
curl https://<host>/api/v1/archive/NixOS/nixpkgs/1
maintainer-mode archive export --repo NixOS/nixpkgs --out ./export
```

The export writes one `ndjson.zst` file per 10 000 numbers. To archive more
repositories, add them to `services.maintainer-mode.archive.repositories`.
An empty list turns the archive off.

## The git copy

Maintainer Mode keeps a git copy of each archived repository, next to the
archive of its issues and pull requests. With the two, GitHub is not the
only place that holds the code and its review history.

- **What.** Every branch and tag, and the head of every pull request
  (`refs/pull/<n>/head`), with full history. A deleted branch stays in the
  copy. The test merges of GitHub (`refs/pull/<n>/merge`) are not kept.
- **Replaced heads.** A force push replaces the head of a pull request.
  The copy logs each change of a ref (`core.logAllRefUpdates=always`) and
  never expires the log, so the old head stays:
  `git rev-parse 'refs/pull/<n>/head@{1}'`. GitHub keeps only the newest
  head, so the copy has the old heads from its first fetch on.
- **When.** All branches and tags every 5 minutes, and at once after a push
  event. The head of a pull request about 10 seconds after an event on it
  (nixpkgs only, from the event log). Every branch and every head each 6
  hours, to get what the events missed; an hour after a failed run.
- **Storage.** A bare repository per repository, `<owner>/<name>.git`, in
  `MAINTAINER_MODE_GIT_DIR` (default: `git` next to the database). The
  NixOS module uses its own state directory, `/var/lib/maintainer-mode-git`,
  so that a host can put the copy on a file system without snapshots and
  backups. The refs are in the reftable format.
- **Packing.** The first fetch takes all refs at once, so GitHub sends one
  complete pack. A later fetch gets a thin pack, and git stores a second
  full copy of each delta base next to it: about 350 KB per pull request
  head in a sample. After each full fetch, `git repack --geometric=2`
  merges the loose objects and the small packs, and leaves the large pack
  as it is. A merge does not write again an object that the large pack
  holds, so the second copies go; until a merge, they stay in the newest
  packs. In a test, 300 recent heads took 26 MB as fetched and 3.3 MB after
  the merge, in 12 seconds. Then git writes the commit-graph. The copy
  never prunes and never runs `git gc`, so it deletes no object. A full
  `git repack -a -d` of nixpkgs reads the whole pack again: 6 minutes with
  6.9 GB resident, and more than 25 minutes under a limit of 2 GB. The copy
  does not run it.
- **Size.** In October 2026, NixOS/nixpkgs has 518 562 pull request heads.
  76 % of them are in the history of a branch. The first copy, on
  2026-10-25, took 33 minutes and 5.1 GB, with one pack of 4.7 GB. Its
  `index-pack` uses about 800 MB of memory. The pack that it writes fills
  the file cache up to the memory limit of the service, and the kernel
  frees that cache. git runs with `oom_score_adj` 1000, so the kernel stops
  git, not the server, when memory runs out. A full fetch does not run at
  the same time as a sweep of nixpkgs.
- **Reflogs.** Each change of a ref is an entry in its reflog, with the
  time when the copy made the change. A fetch runs with
  `GIT_REFLOG_ACTION=fetch`, so the message of an entry is short, such as
  `fetch: fast-forward`, and not the whole command line with up to 200
  refspecs. The reflogs start with the first full fetch: `GET /api/v1/sync`
  gives that time as `logs_from`, under `archive`, `git`.
- **Full file lists.** The archive keeps the first 50 files of a pull
  request. For a pull request with more files, the server reads the full
  list from the copy. It finds the merge base of the head and the base
  branch (for a merged pull request, the first parent of its merge commit)
  with `git merge-base`. Then `git diff-tree -r -z --name-only` gives the
  paths from that commit to the head. `diff-tree` compares trees, not the
  contents of files. A list holds up to 10 000 paths. A renamed file gives
  its old path and its new path. After each pass, the task of the copy
  computes up to 50 missing lists, open pull requests first, with a limit
  of 20 seconds on each git call. It keeps each list in memory, for each
  head and base, while the pull request has a row. A restart computes the
  lists again. When the copy does not have the head yet, the task tries
  again after 10 minutes. The next recompute of the pages uses the lists.
  These facts use the full list:
  - the packages, modules and areas that a pull request touches: the
    pull request page, `touches` in the API, and the pull requests on the
    pages of packages, modules and areas;
  - the review lists and For you, which match a pull request to the areas
    of people;
  - the [treewide](rules.md#treewide-pull-requests) mark, and the place of
    a treewide pull request in the lists. A list of 10 000 paths is
    [too large to link](rules.md#pull-requests-too-large-to-link);
  - "no maintainer and no owner", in the queues and in the ledger;
  - the kind of change in the panel of the pull request page;
  - the areas in the metrics, for the pull requests that have a row.

  The `history` facts of older pull requests keep the first 50 files. The
  diff on the pull request page comes from GitHub, at most 100 files.

  A pull request with no row gets its list when its page or
  `GET /api/v1/prs/<number>` asks for it. The limit is 5 seconds on each
  git call. The server keeps up to 256 of these lists in memory, and a miss
  for 10 minutes.

- **Commits of the last year.** Once a day, the task of the copy runs
  `git log --since=<365 days ago> --no-merges --name-only` on master, with
  a limit of 15 minutes. It counts, for the file or the directory of each
  package and NixOS module, the commits that changed a path at or under it.
  The counts stay in memory; a restart counts again. They give the load of
  the [maintainers](rules.md#the-state-of-nixpkgs). On nixpkgs, one year
  takes about 47 seconds.

- **Reading.** `/state?tab=status` and `GET /api/v1/sync` (under `archive`,
  `git`) give the counts, the size, the times and the last error. On the
  server, the service user owns the copy, so root reads it with
  `git -c safe.directory='*' -C /var/lib/maintainer-mode-git/NixOS/nixpkgs.git`.
  `git reflog refs/pull/<n>/head` lists each head that the copy saw.
- **On and off.** The NixOS module turns the copy on; set
  `services.maintainer-mode.archive.git = false` to turn it off. Outside the
  module, the copy is off until `MAINTAINER_MODE_ARCHIVE_GIT=true`, so that
  a local `serve` does not download gigabytes. The copy needs `git` 2.45 or
  later in `PATH`; the module adds it.

## Known limits of slice one

- Each pull request reads its first 30 reviews, the first 5 inline comments of
  each, the last 30 comments, the last 30 pushes and the first 50 files. A very
  busy pull request can lose a trigger or a void past those limits. A
  server with the git copy reads every file of a pull request there: see
  "Full file lists" in [the git copy](#the-git-copy).
- Ownership of a pull request is judged against today's nixpkgs, not the
  nixpkgs of the day of the review.
- The sweep covers derivations in the swept sets, and NixOS modules. A package
  outside the swept sets has no page.
- Linking issues and pull requests to a package reads conventions, not
  intent. A title that names no package and touches no file of it is missed.
- Bot authors (`r-ryantm` among them) are never in the queues, so their pull requests
  do not appear.

## The pull request page

`/pr/<number>` shows one nixpkgs pull request in Maintainer Mode. Links to pull requests across the site open this page. A small GitHub icon next to each title opens GitHub.

- **From the `pr` row.** The header and one panel show the title, author, state, draft, base branch, age, size, checks, labels and the disclosed AI use, the queue, the reviews that moved it, and the packages and modules the pull request touches, with their maintainers and the approvals on the current head. [rules.md](rules.md#the-pull-request-page) describes the panel.
- **From the archive.** A pull request with no `pr` row is older than the synced data. Then the page comes from its copy in the [archive](#the-archive-of-issues-and-pull-requests). The server reads that one item by number, the primary key of the archive file, with no scan. The projection of the rows gives the title, author, state, base branch, dates, size, labels and the first 50 files. A line at the top says that the page comes from the archive. The header shows dates, not ages. The panel shows only Reached and What changes. The server finds the touched packages and modules on each view, from the files and the title, with the rules of the rows. For more than 50 files, it first reads the full list from the git copy (see "Full file lists" in [the git copy](#the-git-copy)). The diff works as for the other pull requests. The conversation is the archive copy. The page has no queue, no Needs you, no follow-up plan, no Hide button and no checks of the packaging. These are for the open pull requests of the synced data. A number that the archive does not have as a pull request gets a short page with a link to GitHub.
- **Reached.** For a merged pull request, the row "Reached" tells which branches and channels hold its merge commit, and which do not yet. The server reads it from the [git copy](#the-git-copy) with `git for-each-ref --contains`, and keeps each answer for a minute. The branches follow the flow of the nixpkgs `CONTRIBUTING.md`: `staging`, `staging-next`, then `master` or `release-YY.MM` (`staging-nixos` goes to the trunk directly; another base goes to `master`). Then the channel branches of the trunk: `nixos-unstable-small`, `nixpkgs-unstable` and `nixos-unstable`, or `nixos-YY.MM-small`, `nixpkgs-YY.MM-darwin` and `nixos-YY.MM`. A channel branch moves when its channel moves. A branch that the copy does not have is left out. Each branch that holds the commit also shows how long after the merge it got it, for example "nixos-unstable 3 days after". The time comes from the [reflog](#the-git-copy) of the branch: the oldest entry whose commit holds the merge commit (`git merge-base --is-ancestor`). The search starts at the merge time and asks about few entries; a forced update starts a new search. It is the time when the copy saw the change, up to about 5 minutes after GitHub. A branch that held the commit at the start of its reflog has no time, and a pull request merged before the reflogs start has none. One page view spends at most 40 `git merge-base` calls and 5 seconds on the times. A time never changes, so the server keeps up to 20 000 of them in memory. The API gives the same as `reached`: the commit, `in_copy`, and a `steps` list of `branch`, `channel`, `contains` and `at` (RFC 3339, or `null` when the time is not known). It is `null` for a pull request that is not merged, or on a server without the git copy. Branches change in the copy every 5 minutes, so a new channel shows up to about 6 minutes late.
- **The diff.** The server reads the changed files from the GitHub REST API, at most 100 files. It uses its own tokens only: the GitHub App token, then the bot token. It never uses the tokens that people lend. A binary file or a very large diff shows a note and a link to GitHub. When GitHub gives fewer files than the pull request changes and the [git copy](#the-git-copy) has the full list, the page lists the paths of the other files, up to 1000, each with a link to its diff on GitHub. When the diff cannot be read, the page lists the paths: the full list from the copy, or the paths of the row.
- **The conversation.** The description, the reviews, the inline comments and the comments come from the archive when it has a copy. If the copy is not current, one GraphQL query reads the first 100 of each kind. The page shows the latest 300 reviews and comments. A link opens the rest on GitHub.
- **Who makes the server read GitHub.** Only signed-in people do. Anonymous visitors and the API get the cache or the archive copy. If neither has the data, they get a note and a link to GitHub.
- **Budget.** The archive walk comes first. The server stops live reads on a token that has less than 1000 points left, until its limit resets. The server starts at most 20 live reads a minute. When two people open the same page at the same time, the server reads GitHub once.
- **Cache.** The server keeps the files per head commit and the conversation per update. It keeps them in memory only, up to 32 MB. A restart clears them.
- **Crawlers.** Each number of a pull request in the archive has a page. `/robots.txt` tells crawlers to stay off `/pr/` and `/api/v1/prs/`.
- **Safety.** The page renders Markdown from GitHub without raw HTML. HTML shows as text, except `<details>`, `<summary>` and `<b>` with no attributes. The page closes each of these tags that the text leaves open. Only web and mail links stay. An image shows as a link, so the page loads nothing from other sites.
- **To act**, use "Review the diff on GitHub" or "Open on GitHub" in the page header. Maintainer Mode writes nothing to nixpkgs.
- `GET /api/v1/prs/<number>` gives the same data as JSON. It holds no data about the person who asks. It never makes the server read GitHub. `files_error` or `discussion_error` tells why a part is missing. `paths` is the list of changed paths that the pages use: the full list from the git copy, or the first 50 files. `paths_complete` is false while the list is cut. `from` is `synced` for a pull request with a `pr` row, and `archive` for one from the archive. For one from the archive, `ci`, `mergeable`, `queue` and `checks` are `null`, and `credits` is empty. A number that is not a pull request in the synced data or in the archive gets 404 and `{"error": …, "url": …}`.

```sh
curl https://<host>/api/v1/prs/123456
```
