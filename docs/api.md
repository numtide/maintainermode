# The JSON API

All endpoints are under `/api/v1`. `GET /api/v1` lists them. Each endpoint
only reads, except `POST /api/v1/reports`. No endpoint writes to GitHub.

The API has no compatibility layer. The project changes, and old paths are
removed, not redirected.

## Endpoints

| Endpoint                                      | Returns                                                          |
| --------------------------------------------- | ---------------------------------------------------------------- |
| `GET /api/v1`                                 | the list of endpoints                                            |
| `GET /api/v1/prs`                             | open pull requests in one queue, paged                           |
| `GET /api/v1/prs/<number>`                    | one pull request (see [data.md](data.md))                        |
| `GET /api/v1/people/<login>`                  | the public profile of a login, and what it maintains             |
| `GET /api/v1/packages/<attr>`                 | one package: health, issues and pull requests                    |
| `GET /api/v1/modules/<file>`                  | one NixOS module                                                 |
| `GET /api/v1/areas/<path>`                    | one path of the nixpkgs tree                                     |
| `GET /api/v1/find/<name>`                     | a redirect to the package, module or area of a name              |
| `GET /api/v1/metrics`                         | the state of nixpkgs, month by month                             |
| `GET /api/v1/status`                          | the counts and the rules of the queues                           |
| `GET /api/v1/sync`                            | what the server has read, and how fresh it is                    |
| `GET /api/v1/events`                          | the event log, as Server-Sent Events (see [data.md](data.md))    |
| `GET /api/v1/archive/<owner>/<repo>/<number>` | one stored issue or pull request                                 |
| `GET /api/v1/packages/index`                  | the public package list of the current index (see below)         |
| `POST /api/v1/reports`                        | uploads a strict package report (see [personal.md](personal.md)) |

## Paging

A list answer has these fields:

- `items`: at most `limit` rows. The default is 50, and the maximum is 200.
- `total`: the number of rows in the full list.
- `next_cursor`: the value to send as `cursor` for the next page, or `null`
  on the last page.

The cursor is opaque. A list can change between two requests, so a row can
show twice or not at all.

## `GET /api/v1/prs`

| Parameter | Meaning                                                                                                |
| --------- | ------------------------------------------------------------------------------------------------------ |
| `queue`   | `foryou`, `unowned`, `bumps`, `all` (the default) or `ready`                                           |
| `section` | with `queue=ready`: `ready` (the default), `maintainer_approved`, `security`, `backports` or `waiting` |
| `login`   | whose queue, for `foryou` and `bumps`; for `foryou` only, the default is your session                  |
| `limit`   | rows per page                                                                                          |
| `cursor`  | the `next_cursor` of the page before                                                                   |

The queues:

- **`foryou`**: pull requests that need review and touch what `login`
  maintains, owns in `ci/OWNERS` or changed this year. It needs `login` or a
  session.
- **`unowned`**: pull requests that need review, with no maintainer.
- **`bumps`**: all open version bumps, the oldest first. With `login`, only what
  that login maintains.
- **`all`**: every pull request that needs review, the oldest first.
- **`ready`**: pull requests ready to merge. The other sections are the lists
  of the committer view.

Your subscriptions and tags are private. They count only when `login` is your own
session. Then the answer has `Cache-Control: private, no-store`.

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
- `person`: whether its pull requests are in the queues;
- `dossier` and `credits`: its review record;
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

The CLI reads the same API. `--server` sets the host, and `--json` prints the
raw answer.

| Command                               | Endpoint                                            |
| ------------------------------------- | --------------------------------------------------- |
| `maintainer-mode prs --queue <queue>` | `/api/v1/prs`                                       |
| `maintainer-mode record <login>`      | `/api/v1/people/<login>`                            |
| `maintainer-mode maintains <login>`   | `/api/v1/people/<login>`                            |
| `maintainer-mode check <login>`       | `/api/v1/people/<login>`                            |
| `maintainer-mode show <name>`         | `/api/v1/find/<name>`                               |
| `maintainer-mode metrics`             | `/api/v1/metrics`                                   |
| `maintainer-mode export`              | `/api/v1/packages/index`                            |
| `maintainer-mode submit`              | `/api/v1/packages/index` and `POST /api/v1/reports` |

`prs` takes `--section`, `--login`, `--limit` and `--cursor`. It prints the
next cursor on stderr.

The MCP server has the same data in its tools `prs` and `person` (see
[actions.md](actions.md)).
