# Plan: the archive of issues and pull requests

Status: built in `src/archive.rs`. The walk has not yet run against GitHub
with a real token, so the figures below are estimates from a sample.

## Goal

Keep a full copy of the issues and pull requests of NixOS/nixpkgs, so that
GitHub is not the only place that holds them. This makes it easier to leave
GitHub later. Other repositories of the NixOS organisation come next. Each
one is one more entry in the list.

## What it stores

For each number, the raw GraphQL JSON of the issue or pull request:

- title, body, author, labels, assignees, milestone, state, timestamps;
- all comments;
- all reviews, with their bodies;
- all inline review comments, with `path`, `line`, `originalLine`,
  `diffHunk`, `replyTo` and the commit;
- the changed files of a pull request;
- the timeline: labels, assignments, closes, reopens, references, renames,
  commits, force pushes, review requests, merges.

A connection with more than one page is read to its end. The query asks
for `state` of a pull request and `commit` of a timeline commit under an
alias. The stored JSON has GitHub's own names again.

A number that GitHub does not resolve (a deleted or transferred issue, or a
discussion) gets a row of kind `none`, so the walk does not ask again.

## Storage

One SQLite file per repository: `archive/<owner>/<name>.sqlite`, next to the
main database. The main database stays small, and the `pr` and `issue`
tables and their sync do not change.

| Table    | Holds                                                                                                           |
| -------- | --------------------------------------------------------------------------------------------------------------- |
| `item`   | `number`, `kind` (`issue`, `pull`, `none`), `updated_at`, `fetched_at`, `size`, `data` (the JSON, zstd level 3) |
| `due`    | numbers to fetch, and the earliest time to fetch them                                                           |
| `failed` | numbers whose fetch failed at the smallest size, with the error                                                 |
| `meta`   | the cursors, the watermarks and the counters                                                                    |

The counters (items, bytes) change with each write, so `/sync` does not
scan the table.

## The walk

The walk runs in its own task, apart from the pull request and issue sync.
A pass starts every 5 minutes. Each pass does these steps for each
repository:

1. **Event hints** (nixpkgs only). Read the new events of the event log.
   Each issue or pull request that got an event is due 5 minutes later.
2. **Recent updates.** List the issues and the pull requests by
   `updatedAt`, newest first, 100 numbers per page, down to the last
   watermark. On the first pass, the watermark is 30 days ago. A number
   whose `updatedAt` differs from the stored copy becomes due. The listing
   reads at most 20 pages per kind per pass, and continues from its cursor.
3. **Due items.** Fetch the due numbers, oldest due first.
4. **History.** When the first listing is complete, use the rest of the
   budget to walk back from the highest number to #1. The walk skips the
   numbers that are stored already.

The budget is a fixed number of items per repository per pass
(`MAINTAINER_MODE_ARCHIVE_ITEMS`, default 300). The walk uses only the
GitHub App token and the sync token of the server, through
`github::Client`. It never uses the tokens that people lend when they sign
in. With neither server token, the walk does not run, and `/sync` says so.
It stops for the pass when the token has less than 1000 points left, so
the sync keeps its budget.

All state is in the archive file, so the walk continues after a restart.

A query asks for 10 items. GitHub answers 502 after 10 seconds, so a
failed query is asked again with 5 items, then with 1 item and smaller
first pages. An item that fails at the smallest size is due again one day
later. The `failed` table keeps it, with the error, until a fetch
succeeds. `/sync` shows the count of failed items. The walk is complete
only when every number up to the highest has a row. A failed item keeps the
walk from completion.

The walk does not store the partial data of a failed item. GitHub gives a
null field for each error, and the archive must not hold a copy with holes
that looks complete.

## Reading

- `GET /api/v1/archive/<owner>/<repo>/<number>` gives the stored JSON, or 404.
  The endpoint has no authentication, so the archive keeps public
  repositories only. Each listing asks GitHub for `isPrivate`. For a private
  repository, the walk stops, `/sync` shows the error, and the endpoint
  answers 404.
- `maintainer-mode archive export --repo NixOS/nixpkgs --out <dir>` writes
  `<dir>/NixOS/nixpkgs/NNNNNNN.ndjson.zst`: one file per 10 000 numbers,
  one JSON line per item.
- `/sync` shows, for each repository: the items and the bytes stored, the
  oldest number reached, the percent of the repository, and the time left
  at the current pace. `/api/v1/sync` has the same data.

## Size of nixpkgs

Measured on 16 Oct 2026, without a token.

| Figure                                | Value   | Source                                                              |
| ------------------------------------- | ------- | ------------------------------------------------------------------- |
| highest number                        | 574,897 | `GET /repos/NixOS/nixpkgs/issues?state=all&sort=created&per_page=1` |
| pull requests                         | 509,930 | `GET /search/issues?q=repo:NixOS/nixpkgs+is:pr`, `total_count`      |
| issues                                | 59,241  | `GET /search/issues?q=repo:NixOS/nixpkgs+is:issue`, `total_count`   |
| numbers with no issue or pull request | 5,726   | the difference (about 1 %)                                          |

**Sample.** 14 numbers, spread from #3 to #570000 (12 pull requests, 2
issues). For each, the REST API gave the item, its comments, its reviews
and its inline comments.

| Figure, per item                      | Mean    | Median  |
| ------------------------------------- | ------- | ------- |
| REST JSON                             | 17.9 kB | 10.9 kB |
| REST JSON, `zstd -3` per item         | 3.6 kB  | 3.1 kB  |
| text: bodies, comments, diff hunks    | 3.6 kB  |         |
| comments, reviews and inline comments | 4.4     |         |

The GraphQL JSON has no URL fields for each user, so it is smaller than the
REST JSON. It also has the timeline, which the sample does not have. This
plan uses 10 kB of JSON and 3.5 kB after zstd for each item.

**Estimate for nixpkgs:**

| Figure                               | Estimate                   |
| ------------------------------------ | -------------------------- |
| JSON, not compressed                 | about 6 GB                 |
| archive file (zstd per item, SQLite) | 2 to 3 GB                  |
| export (zstd over 10 000 items)      | less than the archive file |

The sample is small. A single busy item can be 60 kB or more. `/sync`
shows the real bytes when the walk runs, and this table must then be
updated.

## API cost

GitHub computes the cost of a GraphQL query from the `first` of each
connection, divided by 100
([rate limits](https://docs.github.com/en/graphql/overview/rate-limits-and-query-limits-for-the-graphql-api)).
A query of 10 items, with the first 100 comments, timeline items and files
and the first 30 reviews with 30 inline comments each, costs about 4
points. That is about 0.4 points for each item.

| Figure                                          | Estimate                                       |
| ----------------------------------------------- | ---------------------------------------------- |
| first pages, 575,000 items                      | about 230,000 points                           |
| later pages of busy items, listings             | about 10 % more                                |
| total                                           | about 250,000 points                           |
| default budget: 300 items per pass, every 5 min | 3,600 items per hour                           |
| cost of the default budget                      | about 1,440 points per hour, 29 % of 5,000     |
| time for the full walk at the default budget    | about 160 hours, 7 days                        |
| time at 600 items per pass                      | about 80 hours, 3.3 days, 58 % of 5,000 points |
| after the walk: about 220 events per hour       | less than 100 points per hour                  |

A GitHub App installation has at least 5,000 points per hour. The event
rate comes from [docs/events.md](events.md).

## Check the queries

The queries were checked against GitHub's public schema
(<https://docs.github.com/public/fpt/schema.docs.graphql>) with
`graphql-js`. To check them again after a change:

```sh
MM_DUMP=/tmp/q cargo test dump_queries
# then validate /tmp/q/*.graphql against the schema with graphql-js
```

## Open

- Run the walk with the GitHub App token, and replace the estimates with
  the measured figures.
- Add the other repositories of the NixOS organisation.
- A copy of the export outside the host, as for the event log.
