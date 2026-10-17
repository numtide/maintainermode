# Data

How Maintainer Mode reads GitHub and the other sources, what it keeps, and
its known limits.

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
  Mode GitHub App (see [Actions](actions.md)). The sync uses it to read only.
  Signing out deletes your session and its token.
- **The bot token.** A deployment can give the server the classic token of
  its bot account (`MAINTAINER_MODE_BOT_TOKEN_FILE`). The sync uses it before
  the tokens that people lend. The same token reads the notifications of the
  bot account (see [the notifications poller](#the-notifications-poller)) and
  polls the Events API.
- **A GitHub App.** A deployment can sync with the token of a GitHub App
  installation (`GITHUB_APP_ID`, `GITHUB_APP_KEY_FILE`). The service signs a
  JWT with the App's private key and exchanges it for an installation token.
  That token expires after one hour, so the service gets a new one before
  each pass that starts in its last 30 minutes. The App token is used first.
  Install the App on the account that owns it. People also install the App on
  their own repositories, so the service never takes their installations. Set
  `GITHUB_APP_INSTALLATION_ID` to use a different installation.
  The App token comes before the bot token.
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

  No source needs an account. `/state?tab=status` shows what each source did on its last
  pass.

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
- Bot authors (`r-ryantm` among them) are never in the queues, so their pull requests
  do not appear.

## The pull request page

`/pr/<number>` shows one nixpkgs pull request in Maintainer Mode. Links to pull requests across the site open this page. A small GitHub icon next to each title opens GitHub.

- **From the synced data.** The page shows the title, author, state, draft, base branch, labels, age, size, checks, mergeable and the disclosed AI use. It shows the queue and why, the review credit, and the packages and modules the pull request touches. For each package or module, it shows the health, the maintainers, and whether a maintainer approved the current head.
- **The diff.** The server reads the changed files from the GitHub REST API, at most 100 files. It uses its own tokens only: the GitHub App token, then the bot token. It never uses the tokens that people lend. A binary file or a very large diff shows a note and a link to GitHub.
- **The conversation.** The description, the reviews, the inline comments and the comments come from the archive when it has a copy. If the copy is not current, one GraphQL query reads the first 100 of each kind. The page shows the latest 300 reviews and comments. A link opens the rest on GitHub.
- **Who makes the server read GitHub.** Only signed-in people do. Anonymous visitors and the API get the cache or the archive copy. If neither has the data, they get a note and a link to GitHub.
- **Budget.** The sync comes first. The server stops live reads on a token that has less than 1000 points left, until its limit resets. The server starts at most 20 live reads a minute. When two people open the same page at the same time, the server reads GitHub once.
- **Cache.** The server keeps the files per head commit and the conversation per update. It keeps them in memory only, up to 32 MB. A restart clears them.
- **Crawlers.** `/robots.txt` tells crawlers to stay off `/pr/` and `/api/v1/prs/`.
- **Safety.** The page renders Markdown from GitHub without raw HTML. HTML shows as text. Only web and mail links stay. An image shows as a link, so the page loads nothing from other sites.
- **To act**, use "Review the diff on GitHub" or "Open on GitHub" in the page header. Maintainer Mode writes nothing to nixpkgs.
- A pull request that is not in the synced data gets a short page with a link to GitHub.
- `GET /api/v1/prs/<number>` gives the same data as JSON. It holds no data about the person who asks. It never makes the server read GitHub. `files_error` or `discussion_error` tells why a part is missing. A pull request that is not synced gets 404 and `{"error": …, "url": …}`.

```sh
curl https://<host>/api/v1/prs/123456
```
