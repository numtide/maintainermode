# Plan: the nixpkgs event log

Status: proposed. It replaces the separate event pipeline (a message broker and
five processes) with one log inside Maintainer Mode.

## What it does

- Collect every public event on NixOS/nixpkgs.
- Keep them in order, with a numeric id.
- Serve them live and from any past point.
- Keep a copy outside the host, which is also a public dataset.

GitHub sends webhooks only for repositories you administer, so there is no
webhook receiver. The sources are the Events API and GH Archive.

## Sources

- **Events API.** Poll `GET /repos/NixOS/nixpkgs/events` with `ETag`, at the
  interval GitHub asks for in `X-Poll-Interval`. A `304` costs no rate limit.
  The API shows only the last 300 events, so a long gap loses events.
- **GH Archive.** Each hour, read the file for the past hour and add the
  events that the poll missed. This closes the gaps from restarts and bursts.

The GitHub event id is the key, so an event that both sources deliver is
stored once.

## Storage: one SQLite file per month

`events/2026-10.sqlite`, `events/2026-11.sqlite`, and so on. The current month
takes writes. A past month does not change.

| Column       | Why                                                |
| ------------ | -------------------------------------------------- |
| `seq`        | the order on this host; the SSE id                 |
| `id`         | the GitHub event id; unique                        |
| `type`       | `PullRequestEvent`, `IssueCommentEvent`, and so on |
| `created_at` | from GitHub                                        |
| `actor`      | the login                                          |
| `number`     | the pull request or issue, when there is one       |
| `payload`    | the event JSON, zstd-compressed                    |

A file per month keeps each file small, makes retention a file delete, and
makes the backup a file copy. A query over many months attaches the files it
needs.

The `seq` continues across files: a new month starts at the last `seq` plus
one.

## Reading

- `GET /api/v1/events` is Server-Sent Events. With `Last-Event-ID`, it
  replays from that `seq`, then follows live. Filters: `type`, `number`,
  `actor`, and a path prefix for pull requests that touch it.
- The queues read new events at once, so a review shows without waiting for
  the next sync.

## Copy outside the host

Any S3-compatible store, at a provider other than the host's. The default is
bunny.net Storage, in an EU region with the S3 API, with Bunny CDN in front for
the public downloads.

- **Each day:** the events of that day as `YYYY/MM/DD.ndjson.zst`, in the
  GH Archive format. This is the public dataset and the restore path.
- **Each past month:** its SQLite file, compressed, once.

A restore reads the month files, then the daily files of the current month.

The store holds only public events. It needs no access control for reads, and
it can be served in public through the CDN.

## Size

On 16 Oct 2026 the host stored about 200 to 350 events per hour. Before the
store is chosen, one full day of GH Archive gives the real figure and the
payload size, and this section takes those numbers.

## Order

1. One day of GH Archive for NixOS/nixpkgs: count and bytes.
2. The month files and the poll.
3. The GH Archive gap fill.
4. `GET /api/v1/events`.
5. The daily copy to the store.
6. Then remove the old pipeline from the host.

## Open

- How many months stay on the host.
- Whether the bucket is public, or the dataset gets its own URL.
