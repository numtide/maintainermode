# Rules and numbers

This page tells how Maintainer Mode decides what to show, and which pull
requests go in the queues. It also tells how the numbers on /state are made.
The site serves this page at `/rules`.
These rules are the rules of Maintainer Mode only. nixpkgs has its own rules for every contribution: read the [nixpkgs contributor guide](https://github.com/NixOS/nixpkgs/blob/master/CONTRIBUTING.md).

## Maintainer Mode gives no rights

nixpkgs committers give commit access. Maintainer Mode gives them facts that
they do not have today. A person still decides.

A place in the queues is small. Thus a count that only adds, with no
dispute process, is sufficient. Commit access is large. If a number can get
it, people will try to cheat the number, and this design cannot stop that.
Thus nothing here makes a recommendation. No number gives more than a place
in the queues.

The record shows the last fifty reviews of a person, the result of each
review, and whose work they share. Its first number is **pull requests that
a committer merged later, with no more changes after the review**. This
number shows most directly that the judgement of a person agrees with the
people who merge. Nobody can cheat it, because a committer must act for it
to count. The second number is **problems found**: the author changed the
pull request after a review that asked for changes or commented on a changed
line.

The site shows no score and no rank of people. A ranking makes people review
more, not better. Each person has a page with the results of their own
reviews. It shows no count of reviews.

## Inside the nixpkgs rules

Maintainer Mode works inside the nixpkgs
[automation/AI policy](https://github.com/NixOS/nixpkgs/blob/cb05a63e8584e596c5747f368669e7c7ff3615aa/CONTRIBUTING.md#automationai-policy).
It adds no rules of its own to that policy.

- **A person merges.** The policy says that automation without manual review
  "must not be used as the sole arbiter of whether to merge a change". The
  Ready to merge queue is a reading list for a committer. It merges nothing.
- **A counted review is a contribution.** The policy covers reviews. The
  author of a review made with an AI tool must check it and disclose the AI
  use. Maintainer Mode counts reviews by their result. The policy applies to
  them as to all other reviews.
- **The AI numbers are totals.** The [state of nixpkgs](#the-state-of-nixpkgs)
  counts disclosures. It never shows a person or a pull request. The policy
  gives enforcement to people. It asks them to "assume good faith and remain
  civil".

## The rules

The queues need one fact about each review: did it move the pull request?
The ledger (`src/ledger.rs`) keeps this fact. The pages do not show the
ledger. `GET /api/v1/people/<login>` and `GET /api/v1/prs/<n>` give it under
`credits`.

### 1. A review that moves a pull request

A review **moves** a pull request when it is on the pull request of another
person, and one or more of these events occur in the next **14 days**:

| Event                    | The GitHub fact                                                          |
| ------------------------ | ------------------------------------------------------------------------ |
| The author pushed        | a commit on the head, or a force push, after the review                  |
| The author replied       | a comment by the author, or a reply in a review thread, after the review |
| Inline on a changed line | the review has an inline comment on a `+` or `-` line of the diff        |
| It merged                | the pull request merged after the review                                 |

The ledger keeps such a review as a **counted review**. A program reads each
event. No person must judge. Three of the four events need an action by the
other person. Thus one person cannot cause them alone.

**What does not count.** A plain "LGTM". An approval with no comment. An
approval on a pull request that does not merge. A comment on a line that did
not change. A review by a bot. A review of your own pull request.

### 2. Whose pull requests appear

A pull request appears in the queues when its author is **in the queues**.
This means: in the last **60 days**, the author has counted reviews on the
pull requests of **three or more different authors**. Two people who review
each other cannot get to three. A group that is large enough to get to three
is a healthy review community.

After 60 days, a review does not count for a place in the queues.

Your own page `/me/progress` tells you whether your pull requests are in the
queues. Each pull request page tells why it is or is not in a queue.

### 3. Ready to merge

A pull request goes in the **Ready to merge** queue when all of these are
true:

- the checks pass;
- it has no conflict with its base branch;
- a person other than the author approved the current head commit;
- one or more reviews on it are counted reviews.

The other pull requests in the queues go in the review queue, the oldest
first. In the Ready to merge queue, the pull requests of authors with more
recent counted reviews come first. Section 4 tells how the ledger weighs
them.

### 4. How the ledger is calculated

The ledger is a function of public GitHub data and the clock only. Nobody
types data into it. If you delete the database, the walk writes it again from the archive, with the
same ledger. Each counted review has a link to the review.

- **The count only adds.** Nobody loses a counted review. Nobody is banned
  or demoted. Thus there is nothing to appeal, and no dispute queue. A
  person who reviews once and goes away gets no place, and their pull
  request does not appear. That is the only penalty.
- **Decay, not deletion.** For the order of the Ready to merge queue, each
  counted review becomes half as large every 30 days. Decay is the same for
  all persons.
- **Limits.** One counted review for each reviewer on each pull request.
  Five or fewer for each reviewer on each UTC day. The reviews above this
  limit do not count.
- **Less weight for each repeat on one author.** In the window, your first
  counted review on one author has weight 1. The second has 1/2. The third
  has 1/4. Thus two people cannot increase the weight of each other. Nobody
  must decide whether they worked together to cheat.
- **Weight of old pull requests.** A review of a pull request that waited 14
  days or more gets full weight. A review of a new pull request that you
  chose gets 1/2. A review of a pull request with **no maintainer** gets full
  weight at all ages. "No maintainer" means that no `meta.maintainers` and no
  `ci/OWNERS` line covers a file that it changes. A new reviewer can check
  the packaging there.
- **Reciprocity ratio.** The ledger also calculates the part of the weight
  that came from authors who also reviewed the pull requests of that person.
  The API gives this number. The site does nothing with it.
- **The one manual action.** A nixpkgs committer can void a review. To do
  this, they write a comment on the same pull request with the line
  `maintainer-mode: void <link to the review>`. The comment is public, so
  everyone can see the action. The pull request page shows the voided review
  with a link to that comment. "Committer" means that the
  `authorAssociation` of the comment is `OWNER`, `MEMBER` or `COLLABORATOR`.

### The constants, and why

| Constant          | Value | Why                                                                                                                                     |
| ----------------- | ----- | --------------------------------------------------------------------------------------------------------------------------------------- |
| Move window (N)   | 14 d  | A volunteer author has two weekends to answer. A longer window counts a push or merge that has no relation to the review.               |
| Decay half-life   | 30 d  | One month. Two months after a person stops, their weight is one quarter. Shorter punishes a holiday. Longer shows who helped long ago.  |
| Window            | 60 d  | Two half-lives. Older reviews are at 25% and tell little. It also keeps the `pr` table small.                                           |
| Distinct authors  | 3     | The smallest number that two people cannot get to. A higher number stops people who know a small part of nixpkgs well.                  |
| Daily cap         | 5     | A careful nixpkgs review takes 15 to 60 minutes. Five is a full but honest day. Twenty in one hour is not careful.                      |
| Per pull request  | 1     | More rounds on one change are the same work. Without this limit, ten small reviews on one pull request weigh more than one good review. |
| Pair factor       | 1/2   | A real working pair still gets some weight. The total of all repeats stays below 2.                                                     |
| Queue head age    | 14 d  | Most nixpkgs pull requests get their first review in the first two weeks, or never. After that, they are the backlog that needs help.   |
| Fresh-pick factor | 1/2   | People can still review what they know, and it still counts. The backlog has twice the weight.                                          |

`GET /api/v1/status` gives the current values.

## The pull request page

`/pr/<n>` starts with the panel **At a glance**. The panel helps you decide
quickly. It shows only facts that the server has. When a source is missing,
the panel says "unknown". It never guesses a value.

- **Blockers.** A draft, a conflict, failed checks, a reviewer who asked for
  changes and did not approve later, and an unusual base branch. A rebuild
  of 1000 or more packages on master or a release branch blocks. From 500
  rebuilds, the panel tells you to consider staging. These numbers come from
  [the nixpkgs contributor guide](https://github.com/NixOS/nixpkgs/blob/master/CONTRIBUTING.md#changes-causing-mass-rebuilds).
- **What changes.** The kind (new package, update, fix, removal, refactor,
  module change), the version from the title (`hello: 2.12 -> 2.13`), the
  version on master, and the packages and modules that the files touch.
- **Risk.** The `10.rebuild-*` labels for each platform, and the base branch.
- **Upstream.** Only for an update. The `meta.changelog` link, the GitHub
  releases of the source, and a flag when the first number of the version
  changes. For a person who signed in, the server reads the release list of
  the GitHub source once each 30 minutes. It then links the release and the
  compare view, and flags notes that say "breaking".
- **Builds.** The checks of the head commit, your latest nixpkgs-review run
  for this pull request (only for you), and the last Hydra build that a page
  read in the last hour.
- **People.** The author, the maintainers, and the approvals on the current
  head. A label shows when an approval is by a maintainer or a committer.
- **Security.** The CVE ids in the title and the description, with their
  severity and the advisory of the security tracker, with its verdict:
  range unknown, fixed in master, or fixed everywhere. Only for you: a label
  when a package is on your machines.
- **What is missing.** The checks on the lines that the diff adds (hash
  form, `lib.fakeHash`, tests, `versionCheckHook`, the `meta` of a new
  package) and the packaging gaps of the package on master. Failed checks
  come first. Passed checks fold.
- **Actions.** Approve and Request changes open the diff on GitHub. You
  write the review there. Maintainer Mode writes nothing to nixpkgs.

`GET /api/v1/prs/<n>` gives the facts of the panel that are the same for all
people: `decision`, `blockers` (each with `kind`, `level` and `text`) and
`checks` (each with `ok`, `text` and `guide`).

## Routing: what you see, and what counts

The site shows each person the pull requests in their part of nixpkgs. A
review counts only when it moves the pull request. Thus a review outside
your part of nixpkgs counts when it moves something. The site only does not
ask you for it.

The site calculates your part of nixpkgs. You do not choose it. It is the
sum of:

1. **`meta.maintainers`**: each package whose evaluated `meta.maintainers`
   has your GitHub login, at the directory of its `meta.position`. Teams and
   calculated lists count, because the evaluator reads them.
2. **`ci/OWNERS`**: the file that the "Check / owners" job reads. The site
   ignores team lines, because team membership is not public.
3. **What you changed**: the directories of the files in your merged pull
   requests of the last year. The site reads them with your own token when
   you sign in.

You can also add **tags** in `/me/settings`: path prefixes such as
`pkgs/development/python-modules`, for new parts of nixpkgs that you want to
follow.

A pull request has **no maintainer** when no `meta.maintainers`,
`meta.teams` or `ci/OWNERS` line covers a file that it changes. The same test
sets the weight of a review on it, so the label and the ledger agree. These
pull requests have their own tab. A new reviewer can start there: a check of
the packaging (complete `meta`, pinned hashes, `platforms`, a missing
`versionCheckHook`) needs no expert knowledge. Most first reviews find these
problems.

## The state of nixpkgs

`/state` and `GET /api/v1/metrics` show how pull requests move through
nixpkgs, month by month. Each number is a total. No number names a person.

- **Inflow.** The pull requests opened. Also how many came from bots,
  first-time contributors, security fixes, new packages, backports and mass
  rebuilds. "First-time" is the GitHub `authorAssociation` or the nixpkgs
  label `12.first-time contribution`. The other kinds come from the nixpkgs
  labels (`1.severity: security`, `8.has: package (new)`,
  `10.rebuild-linux: 501+`) and from a `release-*` base branch.
- **Packages.** One package for each package name, from the nixpkgs index,
  the health sources and the open pull requests. The numbers are: packages;
  behind upstream; with an open advisory that can affect master (see
  [Affected versions](health.md#affected-versions)), by the worst band of their
  advisories (critical, high, medium, low, no score); with no maintainer (no
  `meta.maintainers` and no `meta.teams`); broken (`meta.broken`); insecure
  (`meta.knownVulnerabilities`); with packaging gaps; and open version bumps
  with their median age in days. The server calculates them after each change of rows
  and after each health update, not on each request. `GET /api/v1/metrics`
  gives them under `packages`. They contain no personal data.
- **Disclosed AI use.** A pull request counts when its last commit or its
  description has an `Assisted-by:` trailer, or when it has the
  `llm-assisted` label. The site counts other forms of AI attribution, such
  as a `Co-authored-by:` trailer, separately. The policy says that these
  forms do not satisfy the disclosure rule. Nobody can see undisclosed use,
  so each count is a minimum. Standard community automation, such as
  `r-ryantm`, is exempt from the policy. It counts under bots.
- **Outflow.** Merged, closed with no merge, the backlog that is open at the
  end of each month, merges with no approval, and self-merges.
- **Latency.** The hours to the first review by another person, and the part
  that got a review in seven days, both for human authors. The days to merge,
  at the median and the 90th percentile.
- **Outcomes by cohort.** Of the pull requests opened in a month and closed
  since, the part that merged: for all, for first-time contributors and for
  disclosed AI use.
- **Load.** In the last 365 days, how many people did half, and 80%, of the
  merges, of the reviews and of the pull requests by humans. This shows how
  few people do the work of nixpkgs, and it names nobody.
- **Directories.** Open, never reviewed, opened and merged pull requests for
  each part of the tree, from the files that a pull request changes.

The ledger reads only the pull requests updated in the last 60 days and all
open ones. These numbers read the facts of every pull request in the
archive: the first 10 reviews, the first 50 files, the labels and
the last commit message, but not the description. They are in a separate
table, so they never go into the ledger. Until the archive walk reaches the
first number, the older months are not complete.
