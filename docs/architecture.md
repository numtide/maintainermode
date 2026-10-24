# Plan: one source for GitHub data, typed HTML, data apart from views

Status: A1 to A5, and B1 to B8 are done.

Two tracks. They touch different code, except `model::Pr`, so they can run
in parallel. Each step is one commit that passes all checks and deploys on
its own.

## Track A: the archive is the one source of PRs and issues

### Today

Two paths read issues and pull requests from GitHub:

| Path                                  | Writes                                                    | Cadence                                    |
| ------------------------------------- | --------------------------------------------------------- | ------------------------------------------ |
| archive walk (`archive::walk`)        | `archive/<owner>/<name>.sqlite`, `pr`, `issue`, `history` | 300 s; a due pass 10 s after each due mark |
| live refresh (`github::refresh_live`) | `pr_live`, `pr.data`                                      | each pass, 500 open PRs                    |

The notifications poller and the event log only mark numbers due.

The archive holds every field of `model::Pr`, `model::Issue` and
`metrics::Facts`, with no caps. Three fields change without a change to
`updatedAt`, so the archive copy goes stale: `mergeable`, the CI state
(`statusCheckRollup`) and `authorAssociation`.

### Target

```
events, notifications, updatedAt listing, open listing, backfill
        │  (mark a number due)
        ▼
one fetcher ──► archive (raw JSON, the record)
        │
        ▼
projection ──► pr, issue, history tables ──► recompute ──► Snapshot
                     ▲
pr_live (mergeable, ci, association) ◄── live refresh of open PRs
```

- The poller and the event poll only mark numbers due. They fetch nothing.
- The fetcher wakes when a number becomes due. It does not wait for the
  300 s pass.
- The projection is a pure function: archive JSON to `Pr`, `Issue` or
  `Facts`. It runs on each archive write and writes the main DB tables.
  `recompute` and every page stay as they are.
- `mergeable`, CI and `authorAssociation` change without a change to
  `updatedAt`. The table `pr_live (number, head_oid, mergeable, ci,
association, checked_at)` keeps them. `project::pr_live` and
  `project::facts_live` join it; `mergeable` and CI apply only to the same
  head.

### Steps

1. **Projection, in shadow.** Add `project::pr`, `project::issue` and
   `project::facts`. They apply the caps that the sync applies today (30
   reviews, 5 inline comments per review, 30 comments, 30 pushes, 50
   files), so the ledger and the queues do not change. A test compares
   the projection with the sync result for recorded items. On the server,
   count the items where the projection and the `pr` row differ, and show
   the count on `/state?tab=status`. Done. Step 5 removes the count: the
   projection is then the only writer of the rows.
2. **Open items first.** Add an "open" listing to the fetcher, so a new
   deployment reads every open PR and issue first. The archive gives
   `open_complete`. Done.
3. **Due marks.** The poller and the event log mark due with no delay
   and wake the fetcher. Done.
4. **`pr_live`.** The live refresh writes `mergeable`, CI and
   `authorAssociation` of the merge candidates and then of 500 open PRs,
   oldest check first, to `pr_live` and to the `pr` row of the same head.
   A full read by the sync or the poller writes `pr_live` too. The shadow
   check compares the live fields where `pr_live` has the same head.
   Done.
5. **Switch.** Each archive write of a nixpkgs item writes its `pr`,
   `issue` and `history` rows, with `pr_live` joined. A row with a newer
   `updated_at` stays. `pr` and `issue` get the open items and the items
   updated in the last 60 days; `history` gets the pull requests created
   since `--history-since`. On the first start, `archive::backfill` writes
   the rows of every stored item, seeds `pr_live` from the open `pr` rows,
   deletes the keys of the old walks and sets `archive_projected`. The sync
   walk, the history walk and the poller's fetch are deleted; the CLI
   `sync` runs archive passes. `issues_complete` is the archive's
   `open_complete`, and the history is complete when the walk reaches #1; items that fail and
   wait for a retry do not hold it back. The
   walk recomputes at most once every 30 s after rows change. Lent tokens
   pay for the listings and due items, and for the live fields of the merge
   candidates, only below `RESERVE` or with no server token, each at most a
   tenth of its budget per rate limit window (`github::Client::lent`).
   Done.
6. **Prune.** Step 5 writes to `pr` only open PRs and PRs updated in the
   last 60 days (`WINDOW_DAYS`). Left: delete the older rows that the sync
   wrote, so `recompute` loads less.
7. **API and docs.** `/api/v1/sync` shows one fetcher. Step 5 removed the
   fields of the old walks (`open_walk_done`, `up_to`, `last_pass`,
   `walk_every_seconds`, `archive[].shadow`) and added
   `rows_from_archive_at`. Left: a review of the status page as a whole.

### Decisions

- **Lent tokens.** A token that a person lends pays for reads that
  benefit that person: items in their interest (their own PRs, the PRs
  and issues of what they maintain, watch or run on their machines, and
  their review queue) and the pages they open. Shared work uses the App
  and bot tokens. A lent token pays for shared freshness only when the
  App and bot budgets are below `RESERVE`, and then for at most 10% of
  its remaining budget. The backfill never uses a lent token. The page
  where a person lends a token states this; that page does not exist yet,
  and `docs/data.md` states the rule.
- **Caps.** The projection keeps the caps of the old sync. Removing them
  changes the ledger, voids and queues, so it is a separate change with
  its own review.
- **Other repositories.** Event hints cover nixpkgs only. Keep it so
  until another repository is added.

### Risks

- A full fetch of a busy item reads every connection to its end and can
  get 502 errors. A due item that fails waits one hour, then one day; an
  item of the backfill waits one day.
- The backfill and the live fetches share the App budget. Live items go
  first; the backfill uses only what is left above `RESERVE`.
- GitHub drops an item when it pages a filtered timeline from a first page
  of fewer than 100 items. The walk reads such a connection again in
  pages of 100. Items stored before that can miss a timeline item; the
  shadow check found 2 in 1132 rows, both in `pushes`. A new update of the
  item fetches it again.

## Track B: typed HTML, and data apart from views

### Today

- Pages are built with `format!` and `push_str` in 294 functions that
  return `String`. `web::esc` is called 342 times. No XSS was found, but
  some spots are safe only because of a rule elsewhere: the subnav label
  (`ui.rs:182`) holds a set name that `sets::check_name` restricts.
- Some data types hold HTML: `committer::Row.why_html`, `mine::Why`
  labels, `profile::ReviewState::badge`.
- MCP sends every tool to `api::answer`, so MCP and API cannot drift.
  The drift is between each page and its `*_value` function. Examples:
  - `catalog::package_page` and `package_value` compute the builds,
    channels, backports and advisories twice.
  - `notify::inbox_page` and `inbox_value` do the same reads.

### Target

- A type `crate::html::Html(String)` with a macro `html!` that escapes
  every argument unless it is `Html`. No new dependency. The output stays
  byte for byte the same, so the exact greps in `nix/test.nix` still
  hold.
- Each feature has:
  - `fn data(app, viewer, query) -> FooView`, with `#[derive(Serialize)]`.
    It holds text, never HTML.
  - `fn view(v: &FooView) -> Html`. It reads no store.
  - `api::answer` returns `serde_json::to_value(foo::data(..))`. MCP gets
    it with no work.

Named types replace the tuples and `json!` values that are built in
several places today:

| Type          | Replaces                                                       |
| ------------- | -------------------------------------------------------------- |
| `SubjectRow`  | `json!` subject rows in catalog, api, prpage and mine          |
| `PackageView` | the `(Jobset, job, Last, dead)` tuples, cache rows, advisories |
| `ReviewList`  | the review page pipeline and `api::prs_value`                  |
| `Blocker`     | `Vec<(Level, String)>` in prpanel; blockers reach the API      |
| `SetView`     | `json!` in `sets_value`                                        |
| `Status` enum | the follow-up status tuple                                     |
| `Pr` (as is)  | `pr_json` and `pr_value`, which copy fields by hand            |

### Steps

1. **`Html` and the ui.rs helpers.** `Page.sub`, `actions`, `body` and
   `heading` become `Html`. A temporary `Html::trusted(String)` lets the
   other modules compile unchanged.
2. **needs.** It is already split. Make `Item` `Serialize` and render it
   with `Html`. Done.
3. **committer.** Remove `why_html`; render `why` in the view. Done.
4. **review and `api::prs_value`.** One `ReviewList`. Done.
5. **notify, sets, mine.** `Inbox`, `SetView`, `Interests::picked`. Done.
6. **profile, home.** Add `SubjectRow`. Done, with `PrSummary`.
7. **prpanel, then prpage.** Blockers and checks reach the API. Done.
8. **catalog.** Split it into package, module, area and maintains, one
   at a time. `PackageView`, `ModuleView`, `AreaView`, `MaintainsView`,
   with `SubjectView` for what they share. Done.
9. **Remove `Html::trusted` and `web::esc`.** Then a string with no
   escaping does not compile.

### Checks for each step

- `src/e2e.rs`: one more test per step that puts `<x>` into each user
  field of the pages of that step.
- `nix/test.nix`: the exact HTML and API greps catch output changes.
- The UX audit at 390 and 1280 px, light and dark, signed in and out.
- An API answer that changes shape updates `docs/api.md` in the same
  commit.

## Order

1. B1, the `Html` type. Small, and every later step uses it.
2. B4, the review list. Done.
3. A1 to A3 and B2, B3, B5 in parallel.
4. A4 to A7, then B6 to B9.
