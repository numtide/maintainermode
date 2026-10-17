# Rules and numbers

How Maintainer Mode decides what to show, what earns credit, and how the
numbers on /state are made. The site serves this page at `/rules`.
These rules decide what Maintainer Mode shows and credits. nixpkgs has its own rules for every contribution: read the [nixpkgs contributor guide](https://github.com/NixOS/nixpkgs/blob/master/CONTRIBUTING.md).

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

A pull request is **unowned** when no `meta.maintainers`, `meta.teams` or
`ci/OWNERS` line covers a file it changes. The same test decides the credit
of a review on it, so the label and the credit agree. It gets its own
bucket, which is the newcomer path: packaging hygiene (complete `meta`,
pinned hashes, `platforms`, a missing `versionCheckHook`) needs no domain
knowledge and is most of what a first review finds.

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
- **Packages.** One package per package name, from the nixpkgs index, the health sources and the open pull requests. The numbers are: packages; behind upstream; with an open advisory, by the worst band of their advisories (critical, high, medium, low, no score); unmaintained (no `meta.maintainers` and no `meta.teams`); broken (`meta.broken`); insecure (`meta.knownVulnerabilities`); with packaging gaps; and open version bumps with their median age in days. The server computes them after each sync pass and after each health update, not on each request. `GET /api/v1/metrics` gives them under `packages`. No personal data goes into them.
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
