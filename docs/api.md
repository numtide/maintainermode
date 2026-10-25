# The JSON API

All endpoints are under `/api/v1`. `GET /api/v1` lists them, with their
parameters and their CLI command. It also lists each page of
the site and the endpoints that give its data. Each endpoint only reads,
except `POST /api/v1/reports` and the two `login` endpoints. No endpoint
writes to GitHub.

The API has no compatibility layer. The project changes, and old paths are
removed, not redirected.

## Endpoints

| Endpoint                                      | Returns                                                          |
| --------------------------------------------- | ---------------------------------------------------------------- |
| `GET /api/v1`                                 | the list of endpoints and pages                                  |
| `GET /api/v1/prs`                             | open pull requests in one queue, paged                           |
| `GET /api/v1/prs/<number>`                    | one pull request (see [data.md](data.md))                        |
| `GET /api/v1/people/<login>`                  | the public profile of a login, and what it maintains             |
| `GET /api/v1/needs`                           | what waits for you (private)                                     |
| `GET /api/v1/mine`                            | your packages, as the filters of `/packages` (private)           |
| `GET /api/v1/sets`                            | your named package sets and their output labels (private)        |
| `GET /api/v1/inbox`                           | your inbox (private)                                             |
| `GET /api/v1/progress`                        | your progress, your goal and your badges (private)               |
| `GET /api/v1/search`                          | what the search box finds: packages, people, PRs, advisories     |
| `GET /api/v1/first`                           | packages for a first contribution, paged                         |
| `GET /api/v1/need-help`                       | packages with an open advisory, the highest risk first, paged    |
| `GET /api/v1/packages/<name>`                 | one package: health, issues and pull requests                    |
| `GET /api/v1/modules/<file>`                  | one NixOS module; `nixos/<name>` also works                      |
| `GET /api/v1/areas/<path>`                    | one path of the nixpkgs tree                                     |
| `GET /api/v1/find/<name>`                     | a redirect to the package, module or area of a name              |
| `GET /api/v1/metrics`                         | the state of nixpkgs, month by month                             |
| `GET /api/v1/status`                          | the counts and the rules of the queues                           |
| `GET /api/v1/sync`                            | what the server has read, and how fresh it is                    |
| `GET /api/v1/events`                          | the event log, as Server-Sent Events (see [data.md](data.md))    |
| `GET /api/v1/archive/<owner>/<repo>/<number>` | one stored issue or pull request                                 |
| `GET /api/v1/packages/index`                  | the public package list of the current index (see below)         |
| `POST /api/v1/reports`                        | uploads a strict package report (see [personal.md](personal.md)) |
| `POST /api/v1/login`                          | starts a `maintainer-mode login`: a device code and a user code  |
| `POST /api/v1/login/poll`                     | the upload token, once you confirm the code on `/login/cli`      |

## Private endpoints

A private endpoint gives your own data only. It reads your session cookie,
or your API token as `Authorization: Bearer <token>`. Make the token in
`/me/settings#api`; the server shows it once and keeps only its hash.
Without either, the answer is 401. The answer always has
`Cache-Control: private, no-store`. No endpoint gives the private data of
another person.

```sh
curl -H "Authorization: Bearer $MAINTAINER_MODE_API_TOKEN" 'https://<host>/api/v1/mine?filter=machines&set=laptop'
```

- **`/api/v1/mine`**: the rows of `/packages?filter=<filter>`. `filter` is
  `mine` (the default), `machines`, `maintain` or `watch`. `set` keeps one of
  your sets, with `filter=machines`. `output`, with `set`, keeps the
  packages of one output label of that set, as `&output=<label>` does on the
  page. An unknown set or output is an error. `q` keeps the names that hold
  the text. Each row has `key`, `name`, `version`, `machine_version` (the
  version that your report gave), `why` (`machine`, `maintain` and `watch`),
  `via_team`, `flags` and `outputs` (the `set` and the `output` label of
  each output that holds it). The answer also has `sets` (the names of your
  sets) and `counts` (the rows of each filter). `filter=all` is the public
  catalog: use `/api/v1/search`.
- **`/api/v1/sets`**: each of your sets, private and public, with their
  packages and their output labels (see
  [`GET /api/v1/sets`](#get-apiv1sets)).
- **`/api/v1/inbox`**: the events of `/inbox`, the newest first. `kind` is
  `mine`, `pr`, `issue`, `advisory`, `release` or `build`. `sub` keeps one subject,
  such as `pkg:hello`. Each event has `unread`. The answer also has `unread`
  (the count in the header) and `subjects`. A read marks nothing read.
- **`/api/v1/progress`**: the data of `/me/progress`: `reviews_week`,
  `reviews_month`, `goal`, `counterparties`, `listed` and `badges`.

## Paging

A list answer has these fields:

- `items`: at most `limit` rows. The default is 50, and the maximum is 200.
- `total`: the number of rows in the full list.
- `next_cursor`: the value to send as `cursor` for the next page, or `null`
  on the last page.

The cursor is opaque. A list can change between two requests, so a row can
show twice or not at all.

## `GET /api/v1/prs`

| Parameter | Meaning                                                                                                         |
| --------- | --------------------------------------------------------------------------------------------------------------- |
| `queue`   | `foryou`, `unowned`, `firsttime`, `bumps`, `all` (the default) or `ready`                                       |
| `section` | with `queue=ready`: `ready` (the default), `yours`, `maintainer_approved`, `security`, `backports` or `waiting` |
| `login`   | whose queue, for `foryou` and `bumps`; for `foryou` only, the default is the caller                             |
| `limit`   | rows per page                                                                                                   |
| `cursor`  | the `next_cursor` of the page before                                                                            |

The queues:

- **`foryou`**: pull requests that need review and touch what `login`
  maintains, owns in `ci/OWNERS` or changed this year. It needs `login`, a
  session or an API token. For your own login, it is For you on `/review`:
  the packages of your machines first, then what you maintain, then what you
  watch.
- **`unowned`**: pull requests that need review, with no maintainer.
- **`firsttime`**: first contributions with no review from another person,
  the oldest first. See [personal.md](personal.md#first-time-authors). The
  author does not need a review record.
- **`bumps`**: all open version bumps, the oldest first. With `login`, only what
  that login maintains; for your own login, also what you watch.
- **`all`**: every pull request that needs review, the oldest first.
- **`ready`**: pull requests ready to merge. The other sections are the lists
  of the committer view. `yours` is the section "In your areas": it needs
  your own login, and you must be a committer. Each row has `why`.

For every queue but `ready`, the parameters `q`, `sort` (`oldest`, `newest`,
`updated`), `checks` (`pass`, `fail`, `pending`), `reviews` (`none`, `some`),
`conflicts` (`hide`) and `base` narrow and order the list, as the filter bar
of `/review` does (see [personal.md](personal.md#review-for-you)). For your
own login, `foryou`, `unowned`, `firsttime` and `all` leave out the pull
requests you hid on the site, and `hidden` gives their count. Each row of
your own `foryou` has `why`: the reasons it is yours, such as
`"you maintain: hello"`.

Your machines, subscriptions and tags are private. They count only when
`login` is the caller. Then the answer has `Cache-Control: private, no-store`.

```sh
curl 'https://<host>/api/v1/prs?queue=unowned&limit=10'
```

## `GET /api/v1/people/<login>`

It gives the public data of one login. It is the data of the profile page
`/u/<login>`:

- `facts`: `committer` (true or false), `packages` and `modules` (the number
  it maintains), `teams` (the `lib.teams` teams that name it) and
  `first_pr_at` (the first pull request in the data, or `null`);
- `open_prs`: its open pull requests. The ones with no review for 7 days or
  more come first, the longest wait first. The others follow, the newest
  first;
- `recent_prs`: its pull requests merged or closed in the last 60 days, at
  most 20, the newest first;
- `areas`: the directories its pull requests of the last year change most,
  with the number of pull requests for each;
- `flakes`: the public flake sets that the login shows on its profile. Each
  has `name`, `reference`, `output`, `source` (`upload` or `server_scan`),
  `updated_at`, `nixpkgs_rev`, `packages` (the nixpkgs attributes),
  `outputs` (the output labels, empty without `--with-outputs`) and
  `package_outputs` (attribute → its labels). A private set is never in it;
- `person`: the ledger summary, and whether its pull requests are in the
  queues (see [Ledger fields](#ledger-fields));
- `dossier`: its review record;
- `credits`: its counted reviews (see [Ledger fields](#ledger-fields));
- `area`: what it maintains, owns and changed;
- `maintains`: its packages and NixOS modules, with their flags.

Each pull request in `open_prs` and `recent_prs` has these fields:

```json
{
  "number": 123456,
  "title": "hello: 2.12.1 -> 2.12.2",
  "url": "https://github.com/NixOS/nixpkgs/pull/123456",
  "state": "OPEN",
  "base": "master",
  "created_at": "2026-10-01T12:00:00Z",
  "merged_at": null,
  "closed_at": null,
  "review": "no_review",
  "checks": "pass",
  "waiting_for_first_review": true
}
```

- `review`: `draft`, `no_review`, `changes_requested`, `approved` or
  `reviewed`. The last approval or change request of each reviewer counts.
- `checks`: `pass`, `fail`, `pending`, or `null` when it is not known.

It never ranks people. Private data, like subscriptions and tags, is never in it.

## Ledger fields

The pages do not show review credit. The ledger still decides the queues.
See [the rules](rules.md#the-rules). The API keeps these fields, with the
same names, for the tools that read them. In a field name, "credit" means a
counted review: a review that moved a pull request.

| Endpoint                     | Field                                                   | What it is                                                               |
| ---------------------------- | ------------------------------------------------------- | ------------------------------------------------------------------------ |
| `GET /api/v1/people/<login>` | `person.listed`, `person.counterparties`                | in the queues, and the number of distinct authors in 60 days             |
| `GET /api/v1/people/<login>` | `person.score`, `person.reciprocity`, `person.credits`  | the decayed weight, the reciprocity ratio, the number of counted reviews |
| `GET /api/v1/people/<login>` | `credits`                                               | each counted review, with its events, its weight and a `dropped` reason  |
| `GET /api/v1/prs/<n>`        | `credits`, `queue.credited_by`                          | the counted reviews on the pull request, and their reviewers             |
| `GET /api/v1/prs`            | `author_score`, `credited_by` on each row (not `bumps`) | the weight of the author, and the reviewers with a counted review        |

Do not use `score` to rank people. It sets only the order of the Ready to
merge queue. The fields stay for compatibility. New tools must not depend on
them.

## `GET /api/v1/needs`

It gives the items of [Needs you](personal.md#needs-you): what waits for you,
the most urgent first. It also gives the follow-up plan of each of your open
pull requests, and the events of your own pull requests (see
[Your pull requests](personal.md#your-pull-requests)). It is private (see
[Private endpoints](#private-endpoints)). Snoozed items are not in it.

`mood` keeps the items of one mood: `all` (the default), `mine`, `machines`,
`review` or `maintain`. `counts` always gives the count of each mood. `mood`
does not change `plans` and `events`.

```json
{
  "login": "alice",
  "mood": "all",
  "counts": { "all": 2, "mine": 1, "machines": 1, "review": 0, "maintain": 0 },
  "items": [
    {
      "key": "adv:curl",
      "cause": "advisory",
      "title": "curl: NIXPKGS-2026-1",
      "detail": "On web-1, laptop. Upstream has 8.10.0.",
      "href": "/p/curl",
      "action": "Open the package",
      "fingerprint": "3f2a9c0d1b7e4a55",
      "moods": ["machines"],
      "labels": ["critical 9.8"],
      "at": null,
      "link": "https://tracker.security.nixos.org/issue/NIXPKGS-2026-1",
      "links": [
        {
          "href": "https://tracker.security.nixos.org/issue/NIXPKGS-2026-1",
          "text": "NIXPKGS-2026-1"
        }
      ],
      "outputs": [
        { "set": "infra", "output": "web-1" },
        { "set": "infra", "output": "laptop" }
      ]
    }
  ],
  "plans": {
    "committer": true,
    "default": { "days": 7, "action": "merge", "source": "built_in" },
    "pull_requests": [
      {
        "number": 575334,
        "title": "hello: 2.12.1 -> 2.12.2",
        "days": 7,
        "action": "merge",
        "source": "pull_request",
        "deadline": "2026-10-24T09:12:00Z",
        "state": "waiting",
        "blocked": [],
        "text": "No review yet. On 2026-10-24 you get a reminder to merge it yourself."
      }
    ]
  },
  "events": [
    {
      "id": "mine-575001-review-3301234567",
      "at": "2026-10-16T08:00:00Z",
      "title": "bob approved: #575001 jq: 1.7.1 -> 1.8.0",
      "url": "https://github.com/NixOS/nixpkgs/pull/575001#pullrequestreview-3301234567"
    }
  ]
}
```

- `cause`: `advisory`, `follow_up`, `reviewed`, `blocked`, `waiting`,
  `review` or `package`. This is also the order of urgency.
- `href`: the page of the primary action, on this site.
- `fingerprint`: it changes when the state behind the item changes.
- `at`: the time of the event behind the item, or `null`. For `follow_up`,
  it is the deadline.
- `link`: the first link of `links`, or `null`.
- `links`: more links, such as the advisory, or the merge box on GitHub and
  the line of the contributor guide.
- `plans.default`: your default plan. `source` is `default` when you saved
  it, and `built_in` when you did not.
- `plans.pull_requests`: one entry for each of your open pull requests.
  - `action`: `ask`, `merge` or `none`.
  - `source`: `pull_request` (set on its page), `default` or `built_in`.
  - `state`: `waiting` (before the deadline), `due`, `reviewed` (another
    person reviewed it), `draft` or `off` (the action is `none`).
  - `blocked`: when `state` is `due` and `action` is `merge`, the conditions
    that stop the merge suggestion, such as `checks fail`.
  - `text`: the sentence that the page of the pull request shows.
- `events`: the events of your own pull requests in the last 30 days, newest
  first. The `id` starts with `mine-<number>-` and ends with `review-<id>`,
  `merged`, `closed` or `due-<date>`.
- `items[].outputs`: for an advisory, the set and the output label of each
  output that runs the package. It is empty for a set without
  [output labels](personal.md#output-labels), and for the other causes.

## `GET /api/v1/sets`

It gives your package sets, as the "On my machines" filter of `/packages`
shows them (see [My package set](personal.md#my-package-set)). It is a
[private endpoint](#private-endpoints).

`set` keeps one set. `output`, with `set`, keeps only the packages of that
output, as `&set=<name>&output=<label>` does on the page. An unknown set or
output, or `output` without `set`, is a 404 with an `error`.

```json
{
  "login": "alice",
  "sets": [
    {
      "name": "infra",
      "source": "upload",
      "public": false,
      "reference": null,
      "output": null,
      "updated_at": "2026-10-20T08:00:00Z",
      "scanned_at": "2026-10-20T07:58:00Z",
      "expires_at": "2027-01-18T08:00:00Z",
      "nixpkgs_rev": "5f2c9e148a3b",
      "dropped": 1,
      "packages": ["curl", "openssl"],
      "outputs": ["web-1", "laptop"],
      "package_outputs": { "curl": ["web-1"], "openssl": ["web-1", "laptop"] },
      "scan": null
    }
  ]
}
```

- `source`: `upload`, `flake_upload` or `server_scan`.
- `reference` and `output`: the flake reference and the flake output of a
  public flake set, or `null`.
- `updated_at`: the time of the last upload or the last good scan.
  `scanned_at`: the time of the scan on your machine.
- `expires_at`: `null` for a flake that the server scans.
- `outputs` and `package_outputs`: the output labels of the set, and the
  labels of each package. They are empty without `--with-outputs`.
- `scan`: the state of the last server scan (`last_at`, `scanned_at` and
  `error`), or `null`.

## `GET /api/v1/search`

It gives what the search box of the site finds for `q`. With `limit`, it
gives at most that many of each kind; the suggestions of the box ask for 5.

- `page`: the page that the site opens at once, or `null`. It is set for a
  name with a page, `@login`, `#NUMBER` or a link to a pull request.
- `results`: packages, NixOS modules and areas, each with `key`, `name`,
  `kind` (`pkg`, `module` or `area`), `url` and
  `flags`.
- `people`: maintainers whose login holds the text, each with `login` and
  `url`.
- `pull_requests`: stored pull requests whose title has the text at the start of a word (3
  characters or more). Open ones come first. Each has `number`, `title`,
  `state`, `draft` and `url`.
- `advisories`: advisories with that id: a security tracker code
  (`NIXPKGS-…`), a CVE or an OSV id (`GHSA-…`). Each has `id`, `title`,
  `url` and `packages`. Words never match an advisory.

## `GET /api/v1/packages/index`

It gives the public package list of the current nixpkgs index. The client
filters a closure with it before an upload (see [personal.md](personal.md)).
It is not paged.

```json
{
  "rev": "<index revision>",
  "attrs": { "hello": ["hello", "2.12.1"] },
  "pnames": { "hello": ["hello"] }
}
```

- `attrs`: for each attribute, its pname and its version.
- `pnames`: for each pname, its attributes. The canonical one comes first.

The JSON is compact. Send `Accept-Encoding: gzip` to get it as gzip. The
`ETag` is the index revision, and `Cache-Control` is `public`. Send the
`ETag` back in `If-None-Match` to get a 304. Before the first index, the
answer is 503.

## The command line

The CLI reads the same API. `--server` sets the host (default
`https://maintainermode.org`, or `MAINTAINER_MODE_SERVER`), and `--json` prints the
raw answer. For a private endpoint, put your API token in
`MAINTAINER_MODE_API_TOKEN`.

| Command                               | Endpoint                                            |
| ------------------------------------- | --------------------------------------------------- |
| `maintainer-mode prs --queue <queue>` | `/api/v1/prs`                                       |
| `maintainer-mode pr <number>`         | `/api/v1/prs/<number>`                              |
| `maintainer-mode record <login>`      | `/api/v1/people/<login>`                            |
| `maintainer-mode maintains <login>`   | `/api/v1/people/<login>`                            |
| `maintainer-mode check <login>`       | `/api/v1/people/<login>`                            |
| `maintainer-mode show <name>`         | `/api/v1/find/<name>`                               |
| `maintainer-mode search <query>`      | `/api/v1/search`                                    |
| `maintainer-mode first`               | `/api/v1/first`                                     |
| `maintainer-mode needs`               | `/api/v1/needs` (private)                           |
| `maintainer-mode mine`                | `/api/v1/mine` (private)                            |
| `maintainer-mode metrics`             | `/api/v1/metrics`                                   |
| `maintainer-mode get <path>`          | any GET endpoint, such as `/api/v1/sets`            |
| `maintainer-mode export`              | `/api/v1/packages/index`                            |
| `maintainer-mode submit`              | `/api/v1/packages/index` and `POST /api/v1/reports` |
| `maintainer-mode login`               | `POST /api/v1/login` and `POST /api/v1/login/poll`  |
| `maintainer-mode why <pname>`         | none: it reads the local report and runs nix        |

`prs` takes `--section`, `--login`, `--limit` and `--cursor`. `mine` takes
`--filter`, `--set`, `--output`, `--query`, `--limit` and `--cursor`. `needs` takes
`--mood`. `prs` and `mine` print the next cursor on stderr. `get` always
prints the JSON.

## Keep the API in sync with the site

Each capability of the site is also in the API and in the CLI. One table in
`src/api.rs` holds them:

- `ENDPOINTS` gives each endpoint: its path, its description, its
  parameters, whether it is private, whether `answer` builds its JSON, and
  its CLI command. When an endpoint has no command, the table says why.
  `GET /api/v1` and the routes of the JSON answers come from it.
- `PAGES` gives each view of the site: a page route, a tab, a filter, a
  section of a page or a section of `/me/settings`. For each view, it gives
  the endpoints that give its data, or why there is none.
- `answer` builds the JSON of each endpoint that this module answers.

`cargo test` fails when:

- a GET page route, or a value of a tab, a filter, a section or a mood, is
  not in `PAGES`;
- a target in `PAGES` is not in `ENDPOINTS`, or has a parameter value that
  the endpoint does not take;
- an `/api/v1` route is not in `ENDPOINTS`, or the reverse;
- an endpoint that this module answers has no builder in `answer`, or a CLI
  command in the table does not exist.

To add a capability:

1. Write a function that gives its JSON, next to the page.
2. Add a row to `ENDPOINTS` with its parameters and its CLI command.
   A private endpoint sets `private: true`.
3. Add a branch to `answer` that calls the function.
4. Add the page, or its tab or filter, to `PAGES`.
5. Add the endpoint to the table at the top of this file.

A public endpoint never gives the private data of
a person.
