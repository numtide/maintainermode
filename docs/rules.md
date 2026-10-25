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
not change. A review by a bot. A review of a pull request that a bot opened,
such as a version update by `r-ryantm`. A review of your own pull request.
[The state of nixpkgs](#the-state-of-nixpkgs) lists the bots.

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

`/pr/<n>` has four parts: the panel **At a glance**, the description, the
files and the conversation. The header says who opened it, into which
branch, and when it was opened and updated, or merged and by whom. The
panel helps you decide quickly. It shows each fact once, and only facts
that the server has. When a source is missing, the panel says "unknown". It
never guesses a value. Details that you need less often fold: the labels,
the touched packages after the first six, the checks that pass, the review
checklist, and why the pull request is in its queue. In the description,
the checklist of the nixpkgs template ("Things done") folds.

- **Reached.** Only for a merged pull request, in the place of the
  blockers: the branches and channels that hold its merge commit (see
  [data.md](data.md#the-pull-request-page)).
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
  come first. Passed checks fold. The review checklist of the nixpkgs docs
  folds below them. Only for an open pull request.
- **In Maintainer Mode.** The queue of the pull request, with a Hide button
  for you. When it is in no queue, the reason (for example a draft). The
  reviews that moved it. Why it is here folds: the authors whose pull
  requests its author reviewed, and why it is in For you.
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
pull requests have their own list on `/review`: Show **No maintainer**. A new
reviewer can start there: a check of the packaging (complete `meta`, pinned
hashes, `platforms`, a missing `versionCheckHook`) needs no expert knowledge.
Most first reviews find these problems.

## The state of nixpkgs

`/state` and `GET /api/v1/metrics` show how pull requests move through
nixpkgs, month by month. Each number is a total. No number names a person.
The page shows charts of complete months, for the last 2 years, the last 5
years (the default) or all of history (`?range=2y`, `?range=all`). Each
chart has a table with its values. The JSON has every month.

- **Bots.** A bot is an account that GitHub marks as a bot, or one of the
  community automation accounts that GitHub lists as users: `r-ryantm` (the
  update bot, nixpkgs-update), `nixpkgs-merge-bot`, `nixpkgs-ci` and
  `github-actions`. The archive keeps only the login of the person who
  merged, so this list is also the bot test for merges. "By people" leaves
  bots out. A merge through the merge bot counts as a merge by a bot, not
  by the maintainer who asked for it. The ledger uses the same test.
- **Does nixpkgs keep up?** The pull requests opened and closed each month,
  the change of the open backlog, the backlog at the end of each month by
  age (under 7 days, 7 to 30, 30 to 90, 90 to 365, a year or more), and the
  backlog in months of work: open at the end of the month, divided by the
  pull requests closed per month over the last three months. Drafts count,
  because nixpkgs keeps no record of when a draft became ready.
- **How long does a pull request wait?** For pull requests by people, each
  month counts the pull requests opened in it, the open ones too: the share
  merged within 7, 30 and 90 days, closed with no merge within 90 days, and
  still open after 365 days. A share shows once its days have passed for the
  whole month, so a recent month does not look faster than it is. Also the
  share with a first review by another person within 7 days. GitHub has
  reviews since 2016-09, so the review numbers start in 2016-10.
- **What changes the wait.** The same shares for kinds of pull requests
  opened in the 12 months that end 90 days or more ago: all by people, the
  first pull request of a new author, the update bot, version updates by
  people (a title like `name: 1.0 -> 1.1`), new packages, NixOS modules with
  no package, security fixes, backports, documentation only, lib or stdenv,
  mass rebuilds, and four sizes of change. A kind with fewer than 20 pull
  requests is left out.
- **Who does the work?** Distinct people each month who opened, reviewed or
  merged a pull request, and new authors (people whose first nixpkgs pull
  request opened that month). The median and the 90th percentile of merges
  per person who merged. The fewest people who did half of the merges, or of
  the reviews, in the 12 months to each month. For new authors: the share
  whose first pull request merged within 90 days, and the share who opened
  another one within 365 days, by whether the first one merged. New-author
  numbers start one year after the first month of the data, because before
  that every author looks new.
- **What arrives?** The pull requests opened by people, by the update bot and
  by other bots; version updates; new packages, security fixes and
  backports; and disclosed AI use. "First-time" is the GitHub
  `authorAssociation` or the nixpkgs label `12.first-time contribution`. It
  counts pull requests, not people: the association stays first-time until
  the first merge. A new package has the label `8.has: package (new)` or a
  title like `name: init at 1.0`; nixpkgs left out the label on most new
  packages from 2025-01 to 2026-02. A security fix has the label
  `1.severity: security`. A mass rebuild has a `10.rebuild-linux:` label of
  501 or more. A backport has a `release-*` base branch.
- **Disclosed AI use.** A pull request counts when its last commit or its
  description has an `Assisted-by:` trailer, or when it has the
  `llm-assisted` label. The site counts other forms of AI attribution, such
  as a `Co-authored-by:` trailer, separately. The policy says that these
  forms do not satisfy the disclosure rule. Nobody can see undisclosed use,
  so each count is a minimum. Standard community automation, such as
  `r-ryantm`, is exempt from the policy. It counts under bots.
- **Where do pull requests wait?** For each part of the tree, from the files
  that a pull request changes: open now, in months of merges (open now,
  divided by the merges per month of the last 365 days), never reviewed, and
  the share of pull requests by people opened from 395 to 30 days ago that
  merged within 30 days. One pull request counts in each part it changes.
- **Load.** In the last 365 days, how many people did half, and 80%, of the
  merges, of the reviews and of the pull requests by people.
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
- **Maintainers.** How the packages (one for each package name) and the
  NixOS modules spread over the people who maintain them. A member of a
  team in `meta.teams` counts as a maintainer. The numbers are: the
  maintainers; the packages and modules with no maintainer, one, two, and
  three or more; and the maintainers by how many packages and modules they
  maintain (1, 2-3, 4-10, 11-30, 31-100, 101 or more), with the share of
  the load that each group carries. The load of a package or a module is 1,
  plus the commits of the last 365 days on master that changed its file or
  its directory. The [git copy](data.md#the-git-copy) gives these commits,
  once a day; merges do not count. The packages that share one file share
  its commits, and the maintainers of a package share its load equally.
  Also the median load, and the fewest maintainers who carry half of the
  load. A server without the git copy counts 1 for each package and
  module. No page and no API shows the load of one person.
  `GET /api/v1/metrics` gives them under `packages.maintainers`.

So that no number points at a person, a share or a percentile of fewer than
20 pull requests or people is null, and a count of people below 3 is null.
The JSON keeps older fields too, such as the days to merge of the pull
requests merged in a month. Those count only merged pull requests, so a
clean-up of old pull requests makes them look slower.

The ledger reads only the pull requests updated in the last 60 days and all
open ones. These numbers read the facts of every pull request in the
archive: the title, the first 30 reviews, the first 50 files, the first 30
labels and the last commit message, but not the description. They are in a
separate table, so they never go into the ledger. Until the archive walk
reaches the first number, the older months are not complete.
