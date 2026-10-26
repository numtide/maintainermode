# Your own view

What Maintainer Mode shows for you: what you maintain, what you watch, and
the packages of your own system.

## The menu

Every page has four tabs: Home, Review, Packages and nixpkgs. Your own pages are in a menu at the top right. Sign in, and the menu shows your login. When you have unread events, the header also shows their number. Push the number to open your inbox. The menu has these items:

- **Get started**, until you hide the card (see below).
- **Inbox** (`/inbox`), with the number of unread events.
- **Progress** (`/me/progress`).
- **Settings** (`/me/settings`).
- **Sign out**.

On these pages, no tab is current. The menu marks the current page.

Only you see `/inbox` and the pages under `/me`. The server sends each answer there with `Cache-Control: private, no-store`. It does the same for `/packages` when you are signed in. `/u/<login>` is public, and it is the same for every viewer. It has three tabs: **Overview**, **Reviews** and **Maintains**. The Reviews tab shows the last reviews of the person and the result of each. It shows no score and no rank. Type `@login` in the search box at the top to open it.

## Needs you

Sign in, and the home page starts with **Needs you**. It is the one list of the home page: a list of open items. Each item is one thing that waits for you. Each item has one primary action: push its title to open its page. The most urgent item comes first. Only you see the list.

The list does not show what happened. The [inbox](#the-menu) does that. The list shows what is still open. When the cause of an item is gone, the item leaves the list.

### The items

| Order | Cause                                                                                                                                                                                                                              | It opens         |
| ----- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------- |
| 1     | An advisory on a package of one of your sets that can affect your version (see [Affected versions](health.md#affected-versions)). The CVSS score is 7 or more, the EPSS chance is 10% or more, or CISA says that attackers use it. | the package page |
| 2     | The deadline of a [follow-up plan](#your-pull-requests) that you chose passed, and nobody reviewed your open pull request.                                                                                                         | the pull request |
| 3     | Your open pull request got a review, a change request or a review comment from another person after your last push or comment.                                                                                                     | the pull request |
| 4     | Your open pull request is blocked: its checks fail, it has a merge conflict, or it has an approval on its last commit for 3 days or more and is not merged.                                                                        | the pull request |
| 5     | Your open pull request has had no review for 7 days or more, and you chose no plan for it. The item uses the built-in default plan (see [Your pull requests](#your-pull-requests)).                                                | the pull request |
| 6     | An open pull request in the review queue changes a package of your machines or a package you maintain. Your review on its last commit removes it.                                                                                  | the pull request |
| 7     | A package you maintain has a newer upstream release, or its last Hydra build on master or a channel, or a dependency of it, failed ([the build watch](health.md#the-build-watch)).                                                 | the package page |

In cause 1, the worst severity comes first, then the highest EPSS chance. In cause 3, the newest review comes first. In causes 2, 4 and 5, the oldest comes first. In causes 6 and 7, the order of [your packages](#your-packages) applies: the packages of your machines come first, then what you maintain. In cause 6, a [treewide](rules.md#treewide-pull-requests) pull request comes after the others, with its mark, such as "treewide · 340 packages". A draft pull request gets cause 3, and cause 4 for failing checks or a merge conflict.

The server uses only data that it already has. These signals are not in the list:

- A review request to you: the synced pull requests do not keep review requests.
- A comment outside a review: the server keeps only the comments of the author.
- The fixed version of an advisory, when neither its ranges nor its title give it. The item then shows an open pull request on the package and the newest upstream release, when they are known.
- A failed Hydra build on a system other than x86_64-linux: [the build watch](health.md#the-build-watch) follows x86_64-linux only.

### Moods

The chips above the list select a mood. Each chip shows its count.

| Chip             | Address           | What it keeps                                          |
| ---------------- | ----------------- | ------------------------------------------------------ |
| All              | `/?mood=all`      | all items                                              |
| My pull requests | `/?mood=mine`     | causes 2, 3, 4 and 5                                   |
| My machines      | `/?mood=machines` | cause 1, and cause 6 for the packages of your machines |
| Review           | `/?mood=review`   | cause 6                                                |
| Maintain         | `/?mood=maintain` | cause 7, and cause 1 for a package you maintain        |

A cookie (`mood`) keeps your last mood for one year.

### Below the list

A row of buttons opens what the list leaves out: your inbox (with the number of unread events), your packages (`/packages?filter=mine`), your open pull requests on GitHub (with their number) and, for a committer, the Ready to merge tab (with its number). Then come the card with your first steps, until you hide it, and, when you maintain nothing, [a first package to work on](#a-first-package-to-work-on). When your maintenance load is below the median, [packages that need help](#packages-that-need-help) come last.

### Snooze

Push "Snooze" to remove an item from the list. The item comes back when its state changes: a new review, a new push, a new advisory, a new release or a new cause. The server keeps the key of the item and a fingerprint of its state. It does not keep more.

### Next

"Start" opens the first item of the mood. On the page of an item, "Next" opens the next item of the same mood. Snoozed items are skipped. The address is `/needs/next?mood=<mood>&from=<key>`. When no item is left, it goes back to the home page. The page of a pull request and the page of a package show "Next" when you are signed in. They also show "Snooze" when the page is an open item.

The footer of the list shows how many items left the list since you signed in, for example "2 done this session". There is no score, no streak and no leaderboard here.

### Keys

On the list and on the pages that "Next" opens, you can use these keys, with [the keys of every page](health.md#keyboard-shortcuts):

| Key          | What it does             |
| ------------ | ------------------------ |
| `j`          | select the next item     |
| `k`          | select the previous item |
| `o` or Enter | open the selected item   |
| `n`          | open the next item       |
| `e`          | snooze the selected item |

The keys do nothing while you type in a field. Everything also works without JavaScript.

The JSON API gives the same list at `GET /api/v1/needs`. See [api.md](api.md#get-apiv1needs).

## Your pull requests

Maintainer Mode tells you what happens to the pull requests that you opened. It can also remind you when nobody reviews one. It never merges and never comments. It shows the facts and links to GitHub. You act on GitHub.

### Events

Your [inbox](#notifications) and your Atom feed show these events of your own pull requests, from the last 30 days. Their kind is "Your pull requests".

| Event                  | When                                                                                  |
| ---------------------- | ------------------------------------------------------------------------------------- |
| a review               | another person approves, requests changes or comments in a review. Bots do not count. |
| merged                 | the pull request is merged. The event names the person who merged it.                 |
| closed                 | the pull request is closed with no merge.                                             |
| the deadline of a plan | the deadline of a plan that you chose passes with no review (see below).              |

Your own reviews, the reviews of bots and dismissed reviews make no event. A comment outside a review makes no event: the synced data keeps only the comments of the author.

The events come from the synced data. The server reads GitHub again at each sync pass, so an event can come some minutes after it happened on GitHub.

### The follow-up plan

The page of your open pull request (`/pr/<number>`) shows the box "Your follow-up plan" under the panel. Only you see it. Set one plan for each pull request:

> If nobody reviews in [N] days: remind me to ask the maintainers / remind me to merge it myself / do nothing.

N is a number from 1 to 90. Push "Save". The box then shows the plan and its deadline, for example "No review yet. On 2026-10-24 you get a reminder to merge it yourself." Push "Use my default" to remove the plan of the pull request.

- A plan that you set on the page counts from the time that you saved it first. A change of the action or of N later does not move that start.
- Your default plan is in `/me/settings#followup`. It counts from the time that the pull request was opened. Before you save a default, the built-in default applies: 7 days, and "remind me to ask the maintainers". For a committer, it is "remind me to merge it myself".
- A review by another person before the deadline cancels the reminder. Its event says "so no reminder comes".
- A draft gets no reminder.

**At the deadline.** When the deadline of a plan that you chose passes, and the pull request is open with no review by another person:

- Needs you shows an item at the top of your pull requests (cause 2).
- Your inbox and your feed get the event.

The built-in default makes no event. It only gives cause 5 of Needs you.

| Action                           | What the item says                                                                                                           |
| -------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| remind me to ask the maintainers | "No review in N days. Ask its maintainers for a review", with at most 5 maintainers of its files                             |
| remind me to merge it myself     | "No review in N days. You can merge it yourself", with a link to the merge box on GitHub and a line of the contributor guide |
| do nothing                       | no item and no event                                                                                                         |

**When the item suggests a merge.** The item says "You can merge it yourself" only when all of these are true:

- This site knows you as a committer (see [For committers](#for-committers)).
- The checks pass.
- GitHub says that it can merge the pull request: there is no merge conflict.
- It is not a draft.
- Nobody requests changes.

When one of them is false, the item says which, for example "Do not merge it yet: checks fail."

**How reliable the committer fact is.** The site counts you as a committer when you merged a pull request in the synced data in the last 365 days, or when you are in the GitHub team `NixOS/nixpkgs-committers`. The server reads the team once a day, and only if its token can see the team. So the fact can be wrong: a new committer with no merge yet is not known when the token cannot see the team, and a person who left the team still counts for a year after a merge. GitHub decides who can merge.

**The contributor guide.** The nixpkgs [CONTRIBUTING.md](https://github.com/NixOS/nixpkgs/blob/master/CONTRIBUTING.md) has no rule about merging your own pull request when nobody reviewed it. The item quotes the nearest line, from [The committer's perspective](https://github.com/NixOS/nixpkgs/blob/master/CONTRIBUTING.md#the-committers-perspective): "To merge, at least one committer has to be confident about its quality." Read the guide before you merge.

**Storage.** The server keeps the number of the pull request, N, the action and the start time for each plan. When the pull request leaves the synced data, the server deletes its plan. Your plans are private: no public page, count or API shows them.

`GET /api/v1/needs` gives your plans and the events of your own pull requests. See [api.md](api.md#get-apiv1needs).

## Your packages

Signed out, `/packages` shows the catalog of nixpkgs: the directories. Signed in, it shows one list: the packages of your machines, what you maintain and what you watch. Only you see it. The chips above the list keep one part of it. Each chip shows its count. The "Words in the name" box filters the list of the current chip, and a chip keeps those words. **Clear** removes them. `/review` has the same bar.

| Filter         | Address                     | What it keeps                                                   |
| -------------- | --------------------------- | --------------------------------------------------------------- |
| All            | `/packages?filter=mine`     | all of the rows below, in one order (the default)               |
| On my machines | `/packages?filter=machines` | the packages of your sets: your machine reports and your flakes |
| I maintain     | `/packages?filter=maintain` | what nixpkgs names you as a maintainer of                       |
| I watch        | `/packages?filter=watch`    | what you watch                                                  |

The tabs **Your packages** and **All of nixpkgs** switch between your list and the catalog, as a visitor sees it, at `/packages?filter=all`. `/packages?q=<text>` with no filter, or with `filter=all`, goes to [the search](health.md#search).

**The order.** The list puts the packages of your machines first. Then it puts what you maintain, then what you watch. In each group, the rows that need attention come first: an open advisory, a newer upstream release, a failed Hydra build or an open pull request that is not [treewide](rules.md#treewide-pull-requests). Then the highest [risk score](health.md#the-risk-score) comes first. Each row shows why it is in the list: "on your machines", "you maintain" or "you watch". A filter does not repeat its own reason on its rows. Each row also shows its health flags. The list shows 50 rows. "Next page" adds `?after=<number>`, the count of rows on the previous pages.

The Hydra flag comes from [the build watch](health.md#the-build-watch), or from a package page that read a failed build in the last hour. The list itself never asks Hydra.

**What each filter adds.**

- **All**: with no machine report, the list links to `/me/settings#upload`: "Upload the packages of your machines". When the list is empty, it also shows the form of I watch.
- **On my machines**: with no report, the list says how to upload one. With a set, a row of set links is above the list (see below). Under the list, the folded box "Your sets" holds a row for each set, [the public flakes](#my-package-set) and what the server keeps.
- **I maintain**: "Watch everything I maintain" at the top right, while you do not watch all of it. When nixpkgs names you nowhere, the list links to maintainers/README.md and to [a first package](#a-first-package-to-work-on).
- **I watch**: a "Stop watching" button on each row, and a form at the bottom of the list to watch a package, a module or a directory by name.

**Pick a set.** The row "Set" shows a link for each set, with its count. "All sets" shows all of them together; this is the default. A set link adds `&set=<name>`, such as `/packages?filter=machines&set=laptop`. The list then shows only the packages of that set. An unknown name shows all sets. In the box "Your sets", each set has one row with its name, its kind, its counts, its upload time and its expiry date. Each row has a "Delete" button. A public flake set also has "Make private" or "Show on my profile".

**Where it runs.** A set that you send with `--with-outputs` (see [My package set](#my-package-set)) knows which output holds each package. Each row of that set then says where the package runs, such as "on web-1, laptop and 2 more". It shows at most three labels. Each label links to the filter of that output. When you pick that set, the row "Where it runs" shows a link for each output, with its count. An output link adds `&output=<label>`, such as `/packages?filter=machines&set=infra&output=web-1`. The list then shows only the packages of that output. `output` works only with `set`. An unknown label shows the whole set.

Needs you on the home page uses the same order for its package items (causes 1, 6 and 7). Below it, "Your packages" links to `/packages?filter=mine`.

**Old addresses.** These addresses redirect:

| Old address        | New address                 |
| ------------------ | --------------------------- |
| `/me`              | `/packages?filter=mine`     |
| `/me?tab=inbox`    | `/inbox`                    |
| `/me/inbox`        | `/inbox`                    |
| `/me?tab=maintain` | `/packages?filter=maintain` |
| `/me?tab=watch`    | `/packages?filter=watch`    |
| `/watch`           | `/packages?filter=watch`    |
| `/me?tab=machines` | `/packages?filter=machines` |
| `/me/sets`         | `/packages?filter=machines` |

## Profiles

`/u/<login>` is the public profile of one person. Every login on the site
links to it. The header shows the GitHub avatar, a link to the GitHub
account, and these facts when the data has them:

- committer: the login merged a pull request in the last year, or is in
  the nixpkgs-committers team;
- the number of packages and NixOS modules that the login maintains;
- the `lib.teams` teams that name the login;
- the date of the first pull request in the synced data.

The page shows facts only. It has no score and no ranking.

| Tab                        | What it shows                                                                                                                             |
| -------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| `/u/<login>`               | Overview: the open pull requests, the merged or closed pull requests of the last 60 days, and the directories of the recent pull requests |
| `/u/<login>?tab=reviews`   | the review record                                                                                                                         |
| `/u/<login>?tab=maintains` | what nixpkgs names the login as a maintainer of                                                                                           |
| `/u/<login>?tab=flakes`    | the public flakes of the login, with their reference, output and packages (see [Public flake sets](#public-flake-sets))                   |

The Overview tab shows the review state, the age and the checks of each open
pull request. Pull requests with no review for 7 days or more come first: a
review helps there. When you look at your own profile, a link goes to your
private pages under `/me`. The profile itself shows no private data.

The Flakes tab shows only when the login has a public flake set. The owner
always sees it, with a note on how to add one.

## Settings

`/me/settings` has four boxes. The links at the top go to each box. Each
section of a box has its own anchor.

| Box                         | Section          | What you set                                                                                     |
| --------------------------- | ---------------- | ------------------------------------------------------------------------------------------------ |
| News (`#news`)              | `#notifications` | the kinds of events that the unread count includes                                               |
|                             | `#feed`          | the private link of your Atom feed                                                               |
|                             | `#followup`      | the default follow-up plan of your pull requests (see [Your pull requests](#your-pull-requests)) |
| Reviews (`#reviews`)        | `#tags`          | the path prefixes that For you on `/review` adds                                                 |
|                             | `#goal`          | your weekly goal                                                                                 |
| Your machines (`#machines`) | `#upload`        | the upload token for `maintainer-mode submit`, with the steps and an example report              |
| Account (`#account`)        | `#api`           | the API token for the `maintainer-mode` command and your own tools (see [api.md](api.md))        |
|                             | `#github`        | nothing: it tells what the server does with the GitHub token of your session                     |
|                             | `#start`         | show or hide the card with your first steps                                                      |

The steps to send your packages fold when you have a set. The example report
and what the server reads with your GitHub token always fold.

## Get started

Signed out, the home page has three parts:

1. A short introduction: what Maintainer Mode is, a link to sign in with GitHub, and a link to the nixpkgs contributor guide (CONTRIBUTING.md). On a phone, it also has a search box, because the header has none.
2. One list: [a first package to work on](#a-first-package-to-work-on).
3. One row of buttons: the pull requests with no maintainer (with their number), the packages and directories (`/packages`), and the state of nixpkgs (`/state`).

After you sign in, the home page shows a card with three steps. Each step shows your real state: done, to do, or optional. The steps depend on whether nixpkgs names you in `meta.maintainers`.

If you maintain nothing yet, the card is **Start contributing**. It teaches you how to start:

1. Make a first review, on the pull requests with no maintainer on `/review`. The step says what to check, and links to the rule that puts your own pull requests in the queues. It is done after your first review.
2. Pick a first package, from the list below. It is done when you watch something.
3. Learn how nixpkgs works: CONTRIBUTING.md, pkgs/README.md, nixos/README.md, maintainers/README.md and the Nixpkgs manual. This step is optional.

If you maintain packages or modules, the card is **Keep up with your packages**. It helps you keep up with them:

1. Watch what you maintain, in the "I maintain" filter of `/packages`. Watch all of it in one step.
2. Review the pull requests on them, in For you on `/review`. It is done after your first review.
3. Get the news: your inbox, or a private Atom feed from `/me/settings#feed`.

### A first package to work on

The home page shows this list to visitors, and to people who maintain
nothing. Big shared packages such as glibc, and issues that have been stuck
for years, are a bad first pick. A package is on the list when all of these
are true:

- It is under `pkgs/by-name`. Core packages with many dependents live
  elsewhere.
- It has no maintainer and no team.
- It is not broken and not insecure. Its risk score is not high.
- It has 3 open issues and pull requests or fewer.
- It has an open issue, or an open pull request that is not a draft and not
  [treewide](rules.md#treewide-pull-requests), from the last 60 days.

Each row gives one task: review the newest pull request, or fix the newest
issue. The newest task comes first. The list shows 8 rows. When no package
fits, it links to the pull requests with no maintainer. After the task,
add yourself to `meta.maintainers`.

### Packages that need help

The home page shows this list to a maintainer whose load is below the
median load of all maintainers (see
[Maintainers](rules.md#the-state-of-nixpkgs)). It lists the packages with
an open advisory in master, the highest risk first, 8 rows. Fix one; when
it has no maintainer, add yourself to `meta.maintainers`.
`GET /api/v1/need-help` gives the whole list. Only you see whether the
list shows on your home page; no page shows your load.

Press "Hide" on the card to remove it. Your menu shows "Get started" until you do. To show the card again, use `/me/settings#start`. Only you see the card. Uploading the packages of your machines is not a step: see [My package set](#my-package-set).

## What I maintain

The "I maintain" filter of `/packages` lists the packages and NixOS modules
whose `meta.maintainers` names you, or whose `meta.teams` names a team you
are in. Each row carries the same health flags as a page. "Watch everything
I maintain" adds every row to what you watch. The list comes from nixpkgs;
change it there.

The **Maintains** tab of `/u/<login>` lists the packages and NixOS modules whose
`meta.maintainers` names the login, or whose `meta.teams` names a team the
login is in. Each row carries the same health flags as a page. The highest
[risk score](health.md#the-risk-score) comes first. The list comes from nixpkgs;
change it there. `/u/<login>/maintains` is public.

## What I watch

Watch any package, module or area from its page, or by name in the "I watch"
filter of `/packages`. Watch everything you maintain in one step in the "I
maintain" filter. The "I watch" filter shows each item with its flags, and a
"Stop watching" button.

- **The Atom feed** is the delivery. `/me/settings#feed` makes a private link,
  `/feed/<token>`, with the last 30 days of what happened to what you watch:
  pull requests opened, merged or closed, issues opened or closed, and
  advisories, when the tracker opens their nixpkgs issue. An advisory with no
  nixpkgs issue shows on the page, not in the feed. Replacing the link cuts
  the old one.
- What you watch is private: it shows only to you and in your feed. The
  package pages are public.
- **Why here.** Accounts, stored preferences and outbound messages are a write
  path. Maintainer Mode already logs people in and keeps state, so a
  subscription is one more row in its database.

## Review, for you

`/review` is the one place to review. It shows one list of pull requests that need a review. The **Show** choice above the list selects which pull requests it holds:

| Show               | Address                 | What it lists                                                                            |
| ------------------ | ----------------------- | ---------------------------------------------------------------------------------------- |
| For you            | `/review?tab=foryou`    | pull requests that touch your machines, what you maintain, own, changed, tagged or watch |
| No maintainer      | `/review?tab=unowned`   | pull requests with no maintainer, team or `ci/OWNERS` line on their files                |
| First-time authors | `/review?tab=firsttime` | first contributions that no one has reviewed (see below)                                 |
| Version bumps      | `/review?tab=bumps`     | open version bumps (see [health.md](health.md))                                          |
| All                | `/review?tab=all`       | every pull request in the queues that is not ready to merge                              |

A committer also sees the tab **Ready to merge** (`/review?tab=ready`, see [For committers](#for-committers)). The address works for everyone.

Sign in, and `/review` shows **For you**. For you looks at the packages of your machines, `meta.maintainers`, `meta.teams`, `ci/OWNERS`, the paths you changed this year, your tags, and every package, module or directory you watch. It uses the order of your packages: pull requests that touch the packages of your machines come first, then those on what you maintain, own, changed or tagged, then those on what you watch. In each group, the oldest comes first. [Treewide](rules.md#treewide-pull-requests) pull requests come after all the others, in the same order. Every review list marks their rows, for example "treewide · 340 packages". A pull request [too large to link](rules.md#pull-requests-too-large-to-link) is never in For you. Signed out, `/review` shows **No maintainer**, and For you is not a choice.

The first row of the list tells in one sentence what the list holds. On No maintainer, "What to check" opens the checks that a first review can do.

Each list shows 50 rows. "Next page" adds `?after=<number>`. On most lists, it is the number of the last pull request on the page. On For you and Ready to merge, it is the count of rows on the previous pages, because these lists are not in number order. `/merge`, `/committer` and `/bumps` redirect to their list.

**Why a pull request is here.** For you, No maintainer and All list only the pull requests of authors who are [in the queues](rules.md#2-whose-pull-requests-appear): in the last 60 days, they reviewed the pull requests of 3 or more other authors. The first row of these lists links to this rule. Each row of For you also says why it is yours, for example "on your machines: curl · you maintain: hello · you watch: nginx". The kinds are: on your machines, you maintain, you own in ci/OWNERS, you changed this year, your tag, and you watch. The page of a pull request shows the same in "Why it is here", with the number of other authors whose pull requests the author reviewed.

**Hide.** Signed in, each row of For you, No maintainer, First-time authors and All has a **Hide** button (key `e`). A hidden pull request leaves these lists and their counts for you only. "N hidden by you" above the list opens `&hidden=1`, the hidden ones of the list, each with **Show again**. The page of a pull request has the same button. A hidden pull request is forgotten when it closes. Ready to merge and Version bumps do not hide anything.

**Sort and filter.** The chips above the list are the Show choice, each with the size of its list once the pull requests you hid are out. A chip keeps the other filters. Under the chips are a search for words in the title, a **Label** field, and **More filters**, which opens the other choices. The Label field suggests the labels of the list with their counts. On a pull request page, each label links to the list of that label. `/packages` has the same bar. It is open when one of them is set. Push **Apply** to show the list. Each choice is a parameter of the address, so you can keep a link to a view:

| Parameter   | Values                                           | What it does                                                            |
| ----------- | ------------------------------------------------ | ----------------------------------------------------------------------- |
| `tab`       | `foryou`, `unowned`, `firsttime`, `bumps`, `all` | the Show choice                                                         |
| `q`         | text                                             | titles with a word that starts with the text                            |
| `sort`      | `oldest`, `newest`, `updated`                    | the order; without it, the order of the Show choice                     |
| `checks`    | `pass`, `fail`, `pending`                        | the state of the checks of the head commit                              |
| `reviews`   | `none`, `some`                                   | no review by another person yet, or one or more                         |
| `conflicts` | `hide`                                           | drops the pull requests with a conflict                                 |
| `base`      | a branch, such as `master` or `staging`          | one base branch; the bar lists the branches of the list                 |
| `label`     | part of a label, such as `python`                | the labels that contain it, in any case; commas for several, all needed |
| `filter`    | `mine`                                           | Version bumps only, signed in: the bumps of what you maintain or watch  |

"Clear" removes them. With a sort, "Next page" counts rows, as on For you. `GET /api/v1/prs` takes the same parameters for every queue but `ready`. For your own login it leaves out what you hid, gives the count as `hidden`, and adds `why` to each row of `queue=foryou`.

The JSON API takes `GET /api/v1/prs?queue=foryou&login=<login>`. The CLI (`maintainer-mode prs --queue foryou --login <login>`) takes the same names. Your machines, subscriptions and tags are private. They count only when your own session or your own API token asks for your login. Then the answer has the rows and the order of For you. In all other cases only your public area counts, in number order. See [api.md](api.md).

### First-time authors

A first contribution that waits for a long time can lose a new contributor. Some wait for a year. Show **First-time authors** lists them, the oldest first. Each row shows how long the author has waited, for example "waiting 11 months".

A pull request is on the list when all of these are true:

- It is open, and it is not a draft.
- GitHub calls its author `FIRST_TIME_CONTRIBUTOR` or `FIRST_TIMER`, or it has the label `12.first-time contribution`.
- The author is not a bot.
- No one other than the author has reviewed it. A review from a bot does not count.

Maintainer Mode does not store the comments of other people. Thus a pull request with a comment but no review stays on the list. The author does not need a review record: this list does not use the queue rules. Everyone can see it, signed in or signed out.

The API takes `queue=firsttime`. The CLI takes `maintainer-mode prs --queue firsttime`. Paging is the same as on the other lists.

## Your progress

`/me/progress` shows your own review record. Only you see it. It never shows the numbers of another person.

- **This week**: the pull requests you reviewed this week, against your weekly goal when you set one, from 1 to 50 in `/me/settings#goal`. A week starts on Monday, in UTC.
- **Over time**: every pull request you reviewed, from the whole archive of nixpkgs, and a chart of the last 12 months. Each pull request counts once, in the month of your first review on it. The archive keeps the first 30 reviews of each pull request.
- **In the queues**: whether your own pull requests show in the queues. Your reviews must move the pull requests of 3 different authors in 60 days. If they show, it gives the day they leave the queues if you review no other author before: the day your third most recent author is 60 days old. If they do not show, it tells how many more authors you need. See [rule 2](rules.md#2-whose-pull-requests-appear).
- **Badges**: the badges you earned, with their date, then the next one with what is left, such as "38 to go". "All badges" lists them all: First review; 10, 50 and 100 reviews; Five parts of nixpkgs; First review with no maintainer; Merged unchanged. The first five read the whole archive. The last two read the last 60 days; once earned, they stay.

Your menu links to the page.

## Notifications

**The inbox.** `/inbox` shows the events of your Atom feed: what
happened in the last 30 days to your own pull requests, to what you watch
and to your package set. The newest event comes first. A pull request that
touches more than one thing you watch gives one event, not one for each
thing. Filter the list by
kind (your pull requests, pull request, issue, advisory, release, Hydra
build) or by subscription. "Mark all read" marks every event as read. Only
you see your inbox.

**Hydra builds.** When the Hydra build of a package that you maintain or
watch turns red or green again, you get an event. See
[the build watch](health.md#the-build-watch).

**A new kind is on.** A kind that Maintainer Mode adds after you saved your
choice is on until you turn it off.

**The unread count.** The header shows the number of unread events. The
server counts them after each sync pass, and when you open the inbox.
In `/me/settings#notifications`, choose the kinds that the count includes.
The kind "Your pull requests" stays on until you turn it off.

## My package set

Tell Maintainer Mode which nixpkgs packages you run, and your feed tells you
when one of them gets an advisory or a new upstream release. Facts about
nixpkgs are public; facts about your machines are not.

**What a set is.** A set is the packages of one upload: all the outputs
that one `scan` read, together. A new upload replaces the whole set of that
name. The useful default is one set for each flake, or for each team that
you work for. Send it with `--with-outputs`, and each package of the set
also says which outputs hold it: the host names and the other outputs of
the flake. Then one set answers "which hosts run this package". One set for
each host is still possible: scan one output with `--output` and send it
with its own `--name`.

You can have up to 50 sets. Each set has a name that you choose, such as
`infra`, `laptop` or `web-1`. A name has 1 to 32 characters: `a-z`, `0-9`,
`-`, `_` and `.`. It starts with a letter or a digit. A set is private,
except a public flake set that you show on your profile.

You evaluate your own tree on your own machine. By default, the server never
evaluates it. `/me/settings#upload` shows these steps with your server's
address:

1. Get the command line tool. It needs Nix with flakes. The first run builds
   it from source, which takes a few minutes.

   ```sh
   nix shell github:numtide/maintainermode
   ```

2. Sign in. The tool shows a link and a code. Open the link, check that the
   page shows the same code, and confirm:

   ```sh
   maintainer-mode login
   ```

   The tool saves a new upload token in
   `~/.config/maintainer-mode/upload-token` (or under `$XDG_CONFIG_HOME`).
   Only you can read the file. A new token replaces the one you had. For CI,
   make a token with the button on `/me/settings#upload` and put it in
   `MAINTAINER_MODE_UPLOAD_TOKEN`.

3. In the repository of your NixOS configuration, read its packages:

   ```sh
   maintainer-mode scan --flake .
   ```

   It reads every output of the flake worth reading. For one machine only,
   name it:

   ```sh
   maintainer-mode scan --flake . --output nixosConfigurations.<host>.config.system.build.toplevel
   ```

4. Send them. `export` is optional: it shows the report before you send it.

   ```sh
   maintainer-mode export
   maintainer-mode submit --name infra
   ```

   Use one name for each flake, such as `infra`. Without `--name`, the set
   is `default`. Add `--with-outputs` to `export` and `submit` to send the
   host names too (see [Output labels](#output-labels)). For another server,
   add `--server https://<server>` to each command.

What each command does:

- **`scan`** reads the closure of one or more flake outputs (`--flake`,
  `--output`), or of the attributes of a file (`--file`, `--attr`), with
  `nix derivation show -r`. `--runtime` reads the built store paths with
  `nix path-info -r` instead. Each output is read on its own. An output that
  `--output` names must read, or the scan stops: a smaller report would
  replace the whole set on upload. An output that the flake gave and that
  does not evaluate here is named and skipped, and the rest still make a
  report.
  Without `--output`, a flake gives every output worth reading on this
  machine: `packages`, `devShells` and `checks` of this system, the
  `config.system.build.toplevel` of each `nixosConfigurations` and
  `darwinConfigurations`, and the `activationPackage` of each
  `homeConfigurations`. `legacyPackages` is never read: it is all of nixpkgs.
  `--ignore` leaves an output out, and takes an attribute path or the start
  of one: `--ignore checks` leaves every check, `--ignore
packages.x86_64-linux.broken` leaves that one. Repeat it for several. It
  works on the outputs `--output` names too. `--runtime` leaves the devShells
  out by itself: a shell has no store path to read, because `mkShell` refuses
  to build one. A check that this machine never built is skipped like any
  other output that it cannot read.
  It reads the nixpkgs revision that the tree
  pins in `flake.lock` or in `npins/sources.json`. `--nixpkgs-input` names
  the pin. It does not evaluate nixpkgs. It writes the full report to
  `maintainer-mode-scan.json`. This file stays on your machine. For each
  package, it also records the outputs that hold it and up to four of its
  derivations (store paths with `--runtime`). For each output, it records a
  label and the nix arguments that name it. These never leave your machine
  without `--with-outputs`, and the paths and the arguments never leave it.
  `--files` is experimental: it also records the nixpkgs files that the
  outputs read, with iets. The upload does not carry them yet. See
  [`docs/file-matching.md`](file-matching.md).
- **`export`** downloads the public package list of `--server` from
  `GET /api/v1/packages/index`. `--index-file` reads a saved copy instead.
  Then it writes the strict report. Read it before you send it.
  `--with-outputs` adds the output labels; see [Output labels](#output-labels).
- **`login`** asks `--server` for a code, then waits while you confirm it
  on `/login/cli`. You must be signed in on the site. Then it saves the new
  upload token. Confirm only a code that your own terminal shows: a person
  who sends you a link with their code gets your upload token. The code
  expires after 10 minutes.
- **`submit`** does the same as `export`, with the list of `--to` (default
  `--server`). Then it sends the strict report to `POST /api/v1/reports`. It
  reads the upload token from `MAINTAINER_MODE_UPLOAD_TOKEN`, from the file
  of `--token-file`, or from the file that `login` saved. `--name` gives the
  name of the set. `--public-flake
REFERENCE#OUTPUT` makes it a public flake set; see
  [Public flake sets](#public-flake-sets). `--with-outputs` works as for
  `export`.
- **`why PNAME`** tells why your outputs hold a package. It reads
  `maintainer-mode-scan.json` (`--report` names another file), finds the
  outputs that hold the package, and runs
  `nix why-depends --derivation <output> <derivation of the package>` for
  each one. `--output LABEL` keeps one output. It prints the chain of
  dependencies of each output. It runs on your machine only and sends
  nothing. Run it in the directory of the scan: it uses the flake reference
  as the scan got it, such as `.`. If the tree changed after the scan, scan
  again.

To save the list for an offline export:

```sh
curl --compressed -o index.json https://<host>/api/v1/packages/index
maintainer-mode export --index-file index.json --out report.json
```

**How the client filters.** The public list gives the pname and the version
of each attribute of the server's current nixpkgs index. The client keeps a
closure entry only when its name is a public attribute or a public pname. It
also tries the name in lower case and `python3Packages.<name>`.

- A kept entry carries the attribute and the pname of the list, not the
  pname of your closure.
- The version goes only when it is equal to the version that the list has
  for that attribute.
- Any other version becomes `null`, with `"modified": true`. A patched
  version, such as `1.2.3-acme-fix`, never leaves your machine.
- The client drops every other name. It sends only their count, as
  `omitted.unknown`.

**What leaves your machine.** The strict report has only these fields:

- `schema`, `client` (the client version) and `generated_at`.
- `source.system`, `source.nixpkgs_rev`, `source.scanned_at` and
  `source.index_rev` (the revision of the public list).
- For each kept entry: `attr`, `pname`, `version` and, if it applies,
  `modified`.
- `omitted.unknown`: the number of dropped names.
- `name`: only when you give `--name`.
- `public_flake.reference` and `public_flake.output`: only when you give
  `--public-flake`.
- `outputs`, and `outputs` in each kept entry: only when you give
  `--with-outputs`. See below.

The name of the scanned tree never leaves your machine. The client never
takes the set name from the tree, the host or the scan. It sends only the
name that you type. The client sends no path, no store hash and no private
name. The local report uses schema 1.

The fold `/me/settings#shared` shows an example. It runs a made-up closure through the
same filter as `submit`. `openssl 3.0.15` goes with its version.
`curl 8.11.1-acme-fix` goes with `"version": null` and `"modified": true`.
`acme-billing` and the name of the tree do not go. The host names `web-1`
and `laptop` go only with `--with-outputs`: the fold shows that report
too, with its risk. The fold also says
what the server keeps and who sees it. It also states the risk: the report
tells which public packages run on your machines and their versions, so a
person who reads it can find the known advisories that can affect you.

**What the server keeps.** It keeps the attributes, the versions that its
index has, the count of dropped names, the scan time and the nixpkgs
revision. Only you see them. They never show on public pages or in public
numbers. The admin of the server can read its database. The server deletes
a report 90 days after the upload, and the "Delete" button of a set deletes it at
once.

**The upload token.** `maintainer-mode login` gets one, or the button in
`/me/settings#upload` makes one. The server shows a token once and keeps
only its SHA-256 hash. You have one token: a new one replaces the old one.
One button revokes it. A token can only send your package sets.

`login` uses a device code. The tool asks `POST /api/v1/login` for a code,
and the server keeps it in memory for 10 minutes. You open `/login/cli`,
sign in if you must (the sign-in brings you back to the page), type the
code that your terminal shows, and confirm it. The page never fills in the
code for you: a link alone cannot confirm a sign-in. Then
the tool asks `POST /api/v1/login/poll` every 2 seconds. After
you confirm, the server gives the token once and forgets the code. A
restart of the server forgets all codes.

**What the server does with a report.**

| Code | Meaning                                                        |
| ---- | -------------------------------------------------------------- |
| 202  | stored; the answer gives the kept and dropped counts           |
| 400  | not a strict report; the error names the problem               |
| 401  | no token, or an unknown token                                  |
| 409  | the name is a flake that the server scans, or you have 50 sets |
| 413  | more than 16 MiB, or more than 50,000 packages                 |
| 503  | the nixpkgs index is not built yet                             |

- It refuses a report with a field outside the strict schema, such as a
  tree name.
- It refuses a set name that does not follow the name rules.
- It refuses a `public_flake` whose reference does not pass the
  public-reference check of [rescan.md](rescan.md#what-the-server-does).
- It refuses a version that its index does not have for that attribute. The
  entry must then have `"version": null` and `"modified": true`.
- It refuses a pname that is not the pname of its index for that attribute.
- It drops an attribute that its index does not have, and counts it. The
  index can change between the download of the list and the upload. A
  broken or hostile client cannot store a private name.
- It keeps one set per name: a new report replaces the set of that name,
  with its flake and its public mark.
- It keeps the name, the attributes, the time of the upload, the time of
  the scan and the nixpkgs revision. For a public flake set, it also keeps
  the reference and the output. With `--with-outputs`, it also keeps the
  output labels and the outputs of each attribute. It keeps nothing else
  from the report.
- **Lifetime.** Each set expires 90 days after its own last upload. The
  background loop deletes it. "Delete" on the row of a set deletes that set
  at once.
- **Access.** Only the owner sees the "On my machines" filter and the
  "on your machines" marks. There is no admin view. No public page, count or
  API uses a private set. The private endpoints `/api/v1/sets` and
  `/api/v1/mine` give your
  sets, with their output labels, to you only (see
  [api.md](api.md#private-endpoints)). A public flake set shows only on the profile and in
  `GET /api/v1/people/<login>`, with its output labels: the outputs of a
  public flake are public.

**Old data.** Before names, a person had one uploaded set and a list of
scanned flakes. The upload became the set `default`, because `submit`
without `--name` still sends to it. Each scanned flake became the set
`flake-<number>`. Both stay private.

**The list.** The "On my machines" filter of `/packages` shows the packages of all your sets, or of the set
that you pick. Each row has the health flags of a package page:
advisories, a newer upstream release, a failed Hydra build, open pull requests and open issues.
A set with output labels also says where each package runs, and filters
by output (see [Your packages](#your-packages)).
The rows that need attention come first. The packages of all sets together also come
first in your packages, in Needs you on the home page and in For you on `/review`.

**Alerts.** Your private Atom feed (see above) also gets, for each package in
your set, a new advisory and a new upstream release. It does not get the pull
requests and issues of the set, because a closure has thousands of packages.

### Output labels

`--with-outputs` on `export` or `submit` sends the label of each output that
the scan read, and the outputs of each kept entry. It is off by default.

| Output                                                   | Label               |
| -------------------------------------------------------- | ------------------- |
| `nixosConfigurations.web-1.config.system.build.toplevel` | `web-1`             |
| `darwinConfigurations.mac.config.system.build.toplevel`  | `mac`               |
| `homeConfigurations."me@box".activationPackage`          | `me@box`            |
| `devShells.x86_64-linux.default`                         | `devShells.default` |
| `packages.x86_64-linux.hello`                            | `packages.hello`    |
| an attribute of `--file`                                 | the attribute       |
| a whole `--file`                                         | the file name       |

A label has 1 to 64 characters: `a-z`, `A-Z`, `0-9`, `-`, `_`, `.`, `@` and
`+`. The scan changes any other character to `_`. Two outputs with the same
label are one output. The report gets one more field at the top, and one in
each entry: the numbers of its outputs in that list.

```json
{
  "outputs": ["web-1", "laptop"],
  "packages": [
    {
      "attr": "openssl",
      "pname": "openssl",
      "version": "3.0.15",
      "outputs": [0, 1]
    },
    {
      "attr": "curl",
      "pname": "curl",
      "version": null,
      "modified": true,
      "outputs": [0]
    }
  ]
}
```

**The risk.** Your host names then leave your machine. A host name can tell
the name of a customer, a project or a place. Use `--with-outputs` only when
you can share these names with the server. `/me/settings#shared` shows the
same example. The paths and the nix arguments of the outputs never leave
your machine.

The server refuses more than 256 labels, a label that does not follow the
rules, a label that is there two times, and an entry that points to no
label. It keeps the labels and the outputs of each kept attribute with the
set. They follow the set: the same owner, the same lifetime and the same
delete. A new upload without `--with-outputs` removes them.

The Needs you items about advisories on your machines name the outputs that
run the package, such as "On web-1, laptop.", in place of "On your
machines.". Their JSON in `GET /api/v1/needs` has `outputs`: a list of
`{ "set", "output" }`.

`GET /api/v1/sets` gives the labels of each set,
and the labels of each package. `GET /api/v1/mine` takes
`set` and `output`, as the page does. See
[api.md](api.md#get-apiv1sets).

### Public flake sets

A public flake set shows on your profile, in the Flakes tab, with its name,
its flake reference, its output and its packages. Other people can see
which nixpkgs packages a public NixOS configuration or package uses. Your
machine reports never show there.

Only a set with a flake reference that passes the public-reference check
can be public: `github:OWNER/REPO`, `github:OWNER/REPO/BRANCH`, or an
`https://` tarball on a public host. See
[rescan.md](rescan.md#what-the-server-does). There are two ways to make one:

- **Upload your own scan.** This works on every server. Scan the public
  flake on your machine, then send it with a name and the reference:

  ```sh
  maintainer-mode scan --flake github:OWNER/REPO --output nixosConfigurations.HOST.config.system.build.toplevel
  maintainer-mode submit \
    --name web-1 --public-flake github:OWNER/REPO#nixosConfigurations.HOST.config.system.build.toplevel
  ```

  The client and the server check the reference. The server checks the
  package names as for each upload. It does not evaluate the flake, so the
  profile says "The server checked the package names, not the flake".

- **Let the server scan it.** This works only when the operator turns
  server rescans on. Give a name, the reference and the output in the
  row "Public flakes" of the box "Your sets", and tick "Show on my profile". When rescans are
  off, the row says so and shows the upload command above.

"Make private" removes a set from your profile and keeps it in your list.
"Show on my profile" makes it public again. A new upload without
`--public-flake` makes the set private and removes its reference.

**A public flake, scanned by the server.** This is off by default. When the
operator turns it on, the box "Your sets" of the "On my machines" filter has a form for a name, a
public flake reference (`github:OWNER/REPO`, `github:OWNER/REPO/BRANCH`, or
an `https://` tarball) and an output. After each nixpkgs index, the server scans the output again.
It keeps the result with the rules of an upload: the same public-list filter, the
same index filter and the same 90-day rule. The server stops the scans of a
person who has not opened `/packages` signed in for 90 days.

- The scan evaluates third-party code. Each nix step runs in a bubblewrap
  sandbox, as a separate process, with a read-only store, its own temporary
  directory, no service state, no secret, and a memory and time limit.
- The evaluation step has no network. The two fetch steps have the network
  of the host, and fetch only the inputs that a checked `flake.lock` names.
- Nix runs with `allow-import-from-derivation = false`: nothing is built.
- A person has at most 3 flakes and 4 scans a day, by default.
- `/state?tab=status` shows the flakes, the queue, the last scan and the failures, as
  counts only.

[`docs/rescan.md`](rescan.md) has the threat model, the sandbox, the
limits, and why the feature is off by default.

## For committers

**Ready to merge** (`/review?tab=ready`) shows what waits for a person with merge rights. A committer who signs in sees it as the second tab of `/review`. Everyone can open the address. The home page of a committer links to it.

**Who is a committer.** Maintainer Mode counts a login as a committer when one of these is true:

- The login merged a pull request in the synced data in the last 365 days.
- The login is a member of the GitHub team `NixOS/nixpkgs-committers`. The server reads the team once a day with its own token. If the token cannot read the team, only the merges count.

`/api/v1/status` shows the counts. No page or API lists the logins.

**The lists.** The tab shows one list. For a committer who signed in, it is **In your areas** when that list has rows. Else it is **Ready to merge**. The list shows 20 rows. "Next page" shows the next 50. Below the list, a row of buttons opens the other lists that have rows, each with its count. Each list has its own page, with 50 rows on each page: `/review?tab=ready&section=<id>`.

| List                     | Id                    | What it shows                                                                                                                             |
| ------------------------ | --------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| In your areas            | `yours`               | only for a committer who signed in: the Ready to merge queue in your areas and in what you watch. Only you see it.                        |
| Ready to merge           | `ready`               | the [Ready to merge queue](rules.md#3-ready-to-merge). Authors with more recent counted reviews come first.                               |
| Approved by a maintainer | `maintainer_approved` | an approval on the current head by a login in the `meta.maintainers` or `meta.teams` of a package or module that the pull request changes |
| Security fixes           | `security`            | the label `1.severity: security`, and ready                                                                                               |
| Backports                | `backports`           | a `release-*` base branch, and ready                                                                                                      |
| Waiting longest          | `waiting`             | ready                                                                                                                                     |

In the last three lists, "ready" means that the checks pass, that there is no known conflict, and that the current head has an approval. The pull request does not need a [counted review](rules.md#1-a-review-that-moves-a-pull-request). The last four lists have no known conflict, and the longest wait comes first. Each of their rows says why it is on the list. The ids are the `section` values of the API.

Maintainer Mode never merges. Each row links to GitHub, and you decide. Read the [nixpkgs contributor guide](https://github.com/NixOS/nixpkgs/blob/master/CONTRIBUTING.md) for how to review and merge.

**The API.** `/api/v1/prs?queue=ready&section=<id>` gives each list. `section=yours` needs your own login, with a session or an API token, because your subscriptions are private.
