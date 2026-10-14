# Maintainer Mode

**What should a nixpkgs maintainer do next, for what we give back?**

Three queues answer it: **pull requests to review**, **pull requests ready to
merge**, and **packages I watch**. A fourth page is your **review record**.
nixfresh answers the same question from
the other end, for what we consume: what is stale, what has an advisory, who
maintains it. nixfresh stays a read-only public dataset. Maintainer Mode is the
only one of the two with accounts.

Review someone else's nixpkgs pull request, and yours gets on the list.

Reviews are cheap and merges are scarce. An approval from someone without
commit access moves nothing, so a club that only trades reviews builds a nicer
waiting room. The product here is the **ready-to-merge queue**: pull requests
that carry a review that changed something, pass their checks, have no
conflict, and have an approval on their current head. A committer can work it
from the top down. The review queue exists to feed it.

Maintainer Mode reads GitHub. **It never writes to GitHub**: no reviews, no
merges, no comments, no labels.

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

## Packages I watch

Follow a package, and the watch page shows its open advisories, a newer
upstream release, and the open nixpkgs pull requests against it.

- **Advisories and upstream** come from a nixfresh instance's public
  `GET /api/v1/packages/<pname>` (`MAINTAINER_MODE_NIXFRESH_URL`). Maintainer
  Mode never writes to nixfresh.
- **Open pull requests** come from Maintainer Mode's own sync: a title that
  starts `<pname>:` (or `….<pname>:`), or a file under `pkgs/by-name/<xx>/<pname>/`.
- **Why here and not in nixfresh.** A public nixfresh instance holds no
  credential and no user table by design. Accounts, stored preferences and
  outbound messages are a write path. Maintainer Mode already logs people in
  and keeps state, so a subscription is one more row in its database. One
  login, one account system, one place that can send a person anything.
- **Nothing is sent yet.** This slice models the subscription and shows what a
  watcher would hear about. Delivery, mail or otherwise, is a later decision.

## Three audiences

| Audience | Surface                                                                                                                                |
| -------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| Humans   | the web page: ready to merge, needs review (your area, unowned, all), packages I watch, one review record per person                   |
| Agents   | `GET /api/v1/queue/merge`, `/api/v1/queue/review?bucket=…&login=…`, `/api/v1/people/<login>` (record, credits, area), `/api/v1/status` |
| Programs | `maintainer-mode check <login>`: exit 0 listed, 3 not listed, 1 broken                                                                 |

```sh
maintainer-mode --server https://<host> merge
maintainer-mode --server https://<host> review --bucket mine --login <you>
maintainer-mode --server https://<host> record <login>
maintainer-mode --server https://<host> check <login>; echo $?
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
- **Tokens.** When you log in, Maintainer Mode keeps your token and reads public
  nixpkgs data with it. The rate limit grows with the number of people who use
  it. The OAuth app asks for no scope, so the token reads public data only. A
  deployment may also give the server a token of its own
  (`MAINTAINER_MODE_SYNC_TOKEN_FILE`), which is used first. Signing out deletes
  your session and its token.
- **The nixpkgs index is evaluated, with iets.** Once a day, Maintainer Mode
  unpacks the source tarball of nixpkgs `master` and evaluates every derivation
  in the top level and `python3Packages` (`--index-set`). For each one it keeps
  the package name, the file of `meta.position`, and the GitHub logins in
  `meta.maintainers`. That one sweep maps paths to packages and packages to
  people. It evaluates a local tree, not a flake reference, so the evaluator
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

## Known limits of slice one

- Each pull request reads its first 30 reviews, the first 5 inline comments of
  each, the last 30 comments, the last 30 pushes and the first 50 files. A very
  busy pull request can lose a trigger or a void past those limits.
- Ownership of a pull request is judged against today's nixpkgs, not the
  nixpkgs of the day of the review.
- The sweep covers derivations in the swept sets. NixOS modules, whose
  `meta.maintainers` sits outside a derivation, are not in it yet.
- Bot authors (`r-ryantm` among them) are never listed, so their pull requests
  do not appear.

## Run it

```sh
nix build
GITHUB_TOKEN=… ./result/bin/maintainer-mode sync --db club.sqlite --index
./result/bin/maintainer-mode serve --db club.sqlite
```

| Variable                          | Meaning                                                     |
| --------------------------------- | ----------------------------------------------------------- |
| `MAINTAINER_MODE_LISTEN`          | listen address, default `127.0.0.1:8790`                    |
| `MAINTAINER_MODE_DB`              | SQLite path                                                 |
| `MAINTAINER_MODE_PUBLIC_URL`      | the public URL; the OAuth callback is `<url>/auth/callback` |
| `GITHUB_CLIENT_ID`                | OAuth app client id                                         |
| `GITHUB_CLIENT_SECRET_FILE`       | file holding the OAuth app client secret                    |
| `MAINTAINER_MODE_SYNC_TOKEN_FILE` | optional token owned by the deployment                      |
| `MAINTAINER_MODE_NIXFRESH_URL`    | nixfresh instance for package health on the watch page      |

CI is the flake's `checks`: `nix flake check`.

## What we took, and under which licence

| Package                                                              | Version | Licence | Used for                                                                                            |
| -------------------------------------------------------------------- | ------- | ------- | --------------------------------------------------------------------------------------------------- |
| [`@primer/css`](https://www.npmjs.com/package/@primer/css)           | 22.3.2  | MIT     | `dist/primer.css`, served as-is                                                                     |
| [`@primer/octicons`](https://www.npmjs.com/package/@primer/octicons) | 19.40.0 | MIT     | thirteen 16px SVG icons                                                                             |
| nixfresh `nix.rs` and `attrs.rs`                                     | 2026-10 | MIT     | `src/eval.rs` and `src/attrs.rs`: the evaluator wrapper and the attribute index, copied and trimmed |

Both are fixed-output fetches of the npm tarballs, pinned by their sha512
(`nix/primer.nix`), and their licence files ship with the assets. The pages
are server-rendered HTML; there is no JavaScript.

Version 22 of `@primer/css` no longer ships `Box`, `Label`, `UnderlineNav`,
`Subhead` or `flash`. `src/club.css` rebuilds those few on Primer's own
color tokens, so light and dark mode follow the bundle.

Primer is GitHub's design system, used so that people who arrive from a pull
request page do not learn a second interface. The club is an independent
community project. It is not a GitHub product and is not endorsed by GitHub,
and it does not use GitHub's name or logo as its own.

## Licence

MIT
