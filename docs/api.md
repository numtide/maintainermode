# The JSON API

All endpoints are under `/api/v1`. `GET /api/v1` lists them. Each endpoint
only reads, except `POST /api/v1/reports`. No endpoint writes to GitHub.

The API has no compatibility layer. The project changes, and old paths are
removed, not redirected.

## Endpoints

| Endpoint                                      | Returns                                                            |
| --------------------------------------------- | ------------------------------------------------------------------ |
| `GET /api/v1`                                 | the list of endpoints                                              |
| `GET /api/v1/prs`                             | open pull requests in one queue, paged                             |
| `GET /api/v1/prs/<number>`                    | one pull request (see [data.md](data.md))                          |
| `GET /api/v1/people/<login>`                  | the public record of a login, and what it maintains                |
| `GET /api/v1/packages/<attr>`                 | one package: health, issues and pull requests                      |
| `GET /api/v1/modules/<file>`                  | one NixOS module                                                   |
| `GET /api/v1/areas/<path>`                    | one path of the nixpkgs tree                                       |
| `GET /api/v1/find/<name>`                     | a redirect to the package, module or area of a name                |
| `GET /api/v1/metrics`                         | the state of nixpkgs, month by month                               |
| `GET /api/v1/status`                          | the counts and the rules of the queues                             |
| `GET /api/v1/sync`                            | what the server has read, and how fresh it is                      |
| `GET /api/v1/events`                          | the event log, as Server-Sent Events (see [data.md](data.md))      |
| `GET /api/v1/archive/<owner>/<repo>/<number>` | one stored issue or pull request                                   |
| `POST /api/v1/reports`                        | uploads a redacted package report (see [personal.md](personal.md)) |

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

Your subscriptions are private. They count only when `login` is your own
session. Then the answer has `Cache-Control: private, no-store`.

```sh
curl 'https://<host>/api/v1/prs?queue=unowned&limit=10'
```

## `GET /api/v1/people/<login>`

It gives the public data of one login:

- `person`: whether its pull requests are in the queues;
- `dossier` and `credits`: its review record;
- `area`: what it maintains, owns and changed;
- `maintains`: its packages and NixOS modules, with their flags.

It never ranks people. Private data, like subscriptions, is never in it.

## The command line

The CLI reads the same API. `--server` sets the host, and `--json` prints the
raw answer.

| Command                               | Endpoint                 |
| ------------------------------------- | ------------------------ |
| `maintainer-mode prs --queue <queue>` | `/api/v1/prs`            |
| `maintainer-mode record <login>`      | `/api/v1/people/<login>` |
| `maintainer-mode maintains <login>`   | `/api/v1/people/<login>` |
| `maintainer-mode check <login>`       | `/api/v1/people/<login>` |
| `maintainer-mode show <name>`         | `/api/v1/find/<name>`    |
| `maintainer-mode metrics`             | `/api/v1/metrics`        |
| `maintainer-mode submit`              | `POST /api/v1/reports`   |

`prs` takes `--section`, `--login`, `--limit` and `--cursor`. It prints the
next cursor on stderr.

The MCP server has the same data in its tools `prs` and `person` (see
[actions.md](actions.md)).
