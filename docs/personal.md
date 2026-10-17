# Your own view

What Maintainer Mode shows for you: what you maintain, what you watch, and
the packages of your own system.

## The menu

Every page has four tabs: Home, Review, Packages and nixpkgs. Your own pages are in a menu at the top right. Sign in, and the menu shows your login. When you have unread events, the header also shows their number. Push the number to open your inbox. The menu has these items:

- **Get started**, until you hide the card (see below).
- **Inbox** (`/inbox`), with the number of unread events.
- **Progress** (`/me/progress`).
- **Runs** (`/me/runs`): your workflow runs.
- **Settings** (`/me/settings`).
- **Sign out**.

On these pages, no tab is current. The menu marks the current page.

Only you see `/inbox` and the pages under `/me`. The server sends each answer there with `Cache-Control: private, no-store`. It does the same for `/packages` when you are signed in. `/u/<login>` is public, and it is the same for every viewer. It has two tabs: **Record** and **Maintains**. Search for `@login` on `/packages` to open it.

## Needs you

Sign in, and the home page starts with **Needs you**. It is a list of open items. Each item is one thing that waits for you. Each item has one primary action: push its title to open its page. The most urgent item comes first. Only you see the list.

The list does not show what happened. The [inbox](#the-menu) does that. The list shows what is still open. When the cause of an item is gone, the item leaves the list.

### The items

| Order | Cause                                                                                                                                                                                                                                        | It opens         |
| ----- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------- |
| 1     | An advisory on a package of your latest machine report that can affect your version (see [Affected versions](health.md#affected-versions)). The CVSS score is 7 or more, the EPSS chance is 10% or more, or CISA says that attackers use it. | the package page |
| 2     | The deadline of a [follow-up plan](#your-pull-requests) that you chose passed, and nobody reviewed your open pull request.                                                                                                                   | the pull request |
| 3     | Your open pull request got a review, a change request or a review comment from another person after your last push or comment.                                                                                                               | the pull request |
| 4     | Your open pull request is blocked: its checks fail, it has a merge conflict, or it has an approval on its last commit for 3 days or more and is not merged.                                                                                  | the pull request |
| 5     | Your open pull request has had no review for 7 days or more, and you chose no plan for it. The item uses the built-in default plan (see [Your pull requests](#your-pull-requests)).                                                          | the pull request |
| 6     | An open pull request in the review queue changes a package of your machines or a package you maintain. Your review on its last commit removes it.                                                                                            | the pull request |
| 7     | A package you maintain has a newer upstream release, or its last Hydra build failed.                                                                                                                                                         | the package page |

In cause 1, the worst severity comes first, then the highest EPSS chance. In cause 3, the newest review comes first. In causes 2, 4 and 5, the oldest comes first. In causes 6 and 7, the order of [your packages](#your-packages) applies: the packages of your machines come first, then what you maintain. A draft pull request gets cause 3, and cause 4 for failing checks or a merge conflict.

The server uses only data that it already has. These signals are not in the list:

- A review request to you: the synced pull requests do not keep review requests.
- A comment outside a review: the server keeps only the comments of the author.
- The fixed version of an advisory, when neither its ranges nor its title give it. The item then shows an open pull request on the package and the newest upstream release, when they are known.
- A failed Hydra build that no page asked for in the last hour: the list never asks Hydra.

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

### Snooze

Push "Snooze" to remove an item from the list. The item comes back when its state changes: a new review, a new push, a new advisory, a new release or a new cause. The server keeps the key of the item and a fingerprint of its state. It does not keep more.

### Next

"Start" opens the first item of the mood. On the page of an item, "Next" opens the next item of the same mood. Snoozed items are skipped. The address is `/needs/next?mood=<mood>&from=<key>`. When no item is left, it goes back to the home page. The page of a pull request and the page of a package show "Next" when you are signed in. They also show "Snooze" when the page is an open item.

The footer of the list shows how many items left the list since you signed in, for example "2 done this session". There is no score, no streak and no leaderboard here.

### Keys

On the list and on the pages that "Next" opens, you can use these keys:

| Key          | What it does             |
| ------------ | ------------------------ |
| `j`          | select the next item     |
| `k`          | select the previous item |
| `o` or Enter | open the selected item   |
| `n`          | open the next item       |
| `s`          | snooze the selected item |

The keys do nothing while you type in a field. Everything also works without JavaScript.

The JSON API gives the same list at `GET /api/v1/needs`. See [api.md](api.md#get-apiv1needs).

## Your pull requests

Maintainer Mode tells you what happens to the pull requests that you opened. It can also remind you when nobody reviews one. It never merges and never comments. It shows the facts and links to GitHub. You act on GitHub.

### Events

Your [inbox](#notifications), your Atom feed and your email digest show these events of your own pull requests, from the last 30 days. Their kind is "Your pull requests".

| Event                  | When                                                                                  |
| ---------------------- | ------------------------------------------------------------------------------------- |
| a review               | another person approves, requests changes or comments in a review. Bots do not count. |
| merged                 | the pull request is merged. The event names the person who merged it.                 |
| closed                 | the pull request is closed with no merge.                                             |
| the deadline of a plan | the deadline of a plan that you chose passes with no review (see below).              |

Your own reviews, the reviews of bots and dismissed reviews make no event. A comment outside a review makes no event: the synced data keeps only the comments of the author.

The events come from the synced data. The server reads GitHub again at each sync pass, so an event can come some minutes after it happened on GitHub.

**One email for each event.** In `/me/settings#email`, select "Send the events of your own pull requests at once". This is off by default. The server then sends one email for each new event, some minutes after it sees the event. It sends at most 20 of these emails in 24 hours, and only to a confirmed address. It sends no email for an event that you read in your inbox before. The daily digest still includes these events.

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
- Your inbox, your feed and your email get the event.

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

`GET /api/v1/needs` and the MCP tool `needs` give your plans and the events of your own pull requests. See [api.md](api.md#get-apiv1needs).

## Your packages

Signed out, `/packages` shows the catalog of nixpkgs: a search box and the directories. Signed in, it shows a filter bar on top. Each filter shows its count. The search box filters the list of the current filter.

| Filter         | Address                     | What it shows                                                                |
| -------------- | --------------------------- | ---------------------------------------------------------------------------- |
| Mine           | `/packages?filter=mine`     | all of the rows below, in one order (the default)                            |
| On my machines | `/packages?filter=machines` | the packages of your latest machine report and of your scanned flakes        |
| I maintain     | `/packages?filter=maintain` | what nixpkgs names you as a maintainer of, and "Watch everything I maintain" |
| I watch        | `/packages?filter=watch`    | what you watch, a form to watch more, and a "Stop watching" button on each   |
| All of nixpkgs | `/packages?filter=all`      | the catalog, as a visitor sees it                                            |

A search with no filter, such as the search box of the home page, goes to All of nixpkgs.

**The order.** Mine puts the packages of your machines first. Then it puts what you maintain, then what you watch. In each group, the rows that need attention come first: an open advisory, a newer upstream release, a failed Hydra build or an open pull request. Then the highest [risk score](health.md#the-risk-score) comes first. Each row shows why it is in the list: "on your machines", "you maintain" or "you watch". It also shows its health flags. Each filter shows 50 rows. "Next page" adds `?after=<number>`, the count of rows on the previous pages.

The Hydra flag shows only a build that a package page read in the last hour. The list itself never asks Hydra.

**No machine report yet.** Then the bar shows the step "Upload the packages of your machines" in place of the machines filter. It links to `/me/settings#upload`. When you have a report, the machines filter shows the time of the latest report, the button "Delete my report", the list and the panel of public flakes (see below).

The home page uses the same order. Its section "Your packages that need you" shows the rows of Mine that need attention. It links to `/packages?filter=mine`.

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

The Overview tab shows the review state, the age and the checks of each open
pull request. Pull requests with no review for 7 days or more come first: a
review helps there. When you look at your own profile, a link goes to your
private pages under `/me`. The profile itself shows no private data.

## Settings

`/me/settings` is one page with these sections. The links at the top go to each section.

| Section      | What you set                                                                                     |
| ------------ | ------------------------------------------------------------------------------------------------ |
| `#email`     | the kinds of events that the unread count and the email digest show, and the digest address      |
| `#followup`  | the default follow-up plan of your pull requests (see [Your pull requests](#your-pull-requests)) |
| `#feed`      | the private link of your Atom feed                                                               |
| `#upload`    | the upload token for `maintainer-mode submit`                                                    |
| `#llm`       | the LLM choice (see [actions.md](actions.md#llms-if-you-want-them))                              |
| `#mcp`       | the MCP token                                                                                    |
| `#workflows` | the setup checklist and the repositories where "Review in CI" and "Update with nix-update" run   |
| `#tags`      | the path prefixes that the For you tab of `/review` adds                                         |
| `#goal`      | your weekly goal                                                                                 |
| `#start`     | show or hide the Get started card                                                                |

## Get started

The home page shows a short introduction to visitors who are not signed in. It names the three ways to use Maintainer Mode. It links to the nixpkgs contributor guide (CONTRIBUTING.md). Press "Hide" to remove it. A cookie keeps it hidden for one year.

After you sign in, the home page shows the Get started card. It is a checklist. Each step shows your real state: done, to do, or optional.

1. What you maintain, in the "I maintain" filter of `/packages`. Watch all of it in one step.
2. What you watch. Watch a package, a module or a directory.
3. The review queue for you, on `/review`.
4. Notifications. Make your private Atom feed in `/me/settings#feed`.
5. Your system. Upload the packages of your machines in `/me/settings#upload`. They then come first in `/packages`.
6. Actions. Follow the setup checklist in `/me/settings#workflows`.
7. LLMs. They are off by default. Turn them on in `/me/settings#llm`.
8. The nixpkgs guides: CONTRIBUTING.md, pkgs/README.md, nixos/README.md, maintainers/README.md and the Nixpkgs manual.

If you maintain nothing, the first step changes. It sends you to the pull requests with no maintainer on `/review`, with a checklist for a first review. It also links maintainers/README.md.

Press "Hide" on the card to remove it. Your menu shows "Get started" until you do. To show the card again, use `/me/settings#start`. Only you see the card.

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

`/review` is the one place to review. It has five tabs, and each tab shows its count:

| Tab            | Address               | What it lists                                                                            |
| -------------- | --------------------- | ---------------------------------------------------------------------------------------- |
| For you        | `/review?tab=foryou`  | pull requests that touch your machines, what you maintain, own, changed, tagged or watch |
| No maintainer  | `/review?tab=unowned` | pull requests with no maintainer, team or `ci/OWNERS` line on their files                |
| Bumps          | `/review?tab=bumps`   | open version bumps (see [health.md](health.md))                                          |
| All            | `/review?tab=all`     | every pull request in the queues that is not ready to merge                              |
| Ready to merge | `/review?tab=ready`   | the ready-to-merge queue, and the lists for committers (see below)                       |

Sign in, and `/review` opens on **For you**. For you looks at the packages of your machines, `meta.maintainers`, `meta.teams`, `ci/OWNERS`, the paths you changed this year, your tags, and every package, module or directory you watch. It uses the order of your packages: pull requests that touch the packages of your machines come first, then those on what you maintain, own, changed or tagged, then those on what you watch. In each group, the oldest comes first. Signed out, `/review` opens on **No maintainer**, and For you is not shown.

Each tab shows 50 rows. "Next page" adds `?after=<number>`. On most tabs, it is the number of the last pull request on the page. On For you and Ready to merge, it is the count of rows on the previous pages, because these tabs are not in number order. `/merge`, `/committer` and `/bumps` redirect to their tab.

The JSON API takes `GET /api/v1/prs?queue=foryou&login=<login>`. The MCP tool `prs` and the CLI (`maintainer-mode prs --queue foryou --login <login>`) take the same names. Your subscriptions are private. They count only when your own session or your own MCP token asks for your login. In all other cases only your public area counts. The API never reads your machine report, and it keeps the number order. See [api.md](api.md).

## Your progress

`/me/progress` shows your own review record. Only you see it. It never shows the numbers of another person. There is no leaderboard.

- **This week and this month**: the pull requests you reviewed, and your reviews that earned credit. A week starts on Monday, in UTC.
- **Streak**: the weeks in a row with at least one review that earned credit. A week with no credit yet does not break the streak until the week ends.
- **Weekly goal**: you set the number of pull requests to review each week, from 1 to 50. Set 0 to remove the goal. `/review` shows how far you are.
- **Badges**: First review; First review with no maintainer; Merged unchanged; 10, 50 and 100 reviews; Five parts of nixpkgs.
- **In the queues**: how many more authors you need before your pull requests show in the queues. You need credit from 3 different authors in 60 days.

The page counts the synced data only: the last 60 days and every open pull request. A small box on `/review` shows your week, your streak and your goal. Your menu links to the page.

## Notifications

**The inbox.** `/inbox` shows the events of your Atom feed: what
happened in the last 30 days to your own pull requests, to what you watch
and to your package set. The newest event comes first. Filter the list by
kind (your pull requests, pull request, issue, advisory, release) or by
subscription. "Mark all read" marks every event as read. Only you see your
inbox.

**The unread count.** The header shows the number of unread events. The
server counts them after each sync pass, and when you open the inbox.
In `/me/settings#email`, choose the kinds that the count includes.
The kind "Your pull requests" stays on until you turn it off.

**The email digest.** This is off by default. It works only when the
operator sets up SMTP (see [operations.md](operations.md#email)). Enter
an address in `/me/settings#email` to get the digest. The server sends at most
one email a day. The email lists the unread events of your chosen kinds
since the last digest. "Send me a test email" sends one email, at most
once in 10 minutes.

- The address is private. It shows only to you, in `/me/settings#email`.
- The server accepts only a plain address, like `name@example.org`. It
  refuses spaces, names and quotes.
- Each email has an unsubscribe link with a new random token. The server
  keeps only the hash of the token, for 60 days. The link opens a page
  with one button. Mail clients that know RFC 8058 unsubscribe in one
  click. Unsubscribing deletes the address.
- To stop the digest, you can also empty the address in `/me/settings#email`.

### Confirm your address

The server sends no digest to an address that you did not confirm.
When you enter or change the address in `/me/settings#email`, the server stores it as pending.
The server then sends one confirmation email to the address.
Open the link in that email in the next 24 hours.
Push the Confirm button on the page that opens.
The settings page then shows "confirmed".
A link scanner can open the link, but it cannot confirm the address. Only the button confirms it.
If the link is too old, push "Send the confirmation email again" on the settings page.
You get at most 3 confirmation emails in 24 hours.
A test email goes only to a confirmed address.
You can send at most 3 test emails in 24 hours, with 10 minutes between them.
Addresses that you saved before this change are pending. Confirm them again to get the digest.

## My package set

Tell Maintainer Mode which nixpkgs packages you run, and your feed tells you
when one of them gets an advisory or a new upstream release. The set is
private. Facts about nixpkgs are public; facts about your machines are not.

You evaluate your own tree on your own machine. By default, the server never
evaluates it. `/me/settings#upload` shows these steps with your server's
address:

1. Get the command line tool. It needs Nix with flakes. The first run builds
   it from source, which takes a few minutes.

   ```sh
   nix shell github:numtide/maintainermode
   ```

2. Make an upload token on `/me/settings#upload`. The server shows it once.
   Save it in a file that only you can read:

   ```sh
   mkdir -p ~/.config/maintainer-mode
   (umask 077; cat > ~/.config/maintainer-mode/upload-token)
   ```

   Paste the token, then push Enter and Ctrl-D. You can also put the token in
   `MAINTAINER_MODE_UPLOAD_TOKEN`.

3. In the repository of your NixOS configuration, read the packages of a
   machine. Repeat `--output` for more machines:

   ```sh
   maintainer-mode scan --flake . --output nixosConfigurations.<host>.config.system.build.toplevel
   ```

4. Send them. `export` is optional: it shows the report before you send it.

   ```sh
   maintainer-mode --server https://<server> export
   maintainer-mode submit --to https://<server> --token-file ~/.config/maintainer-mode/upload-token
   ```

What each command does:

- **`scan`** reads the closure of one or more flake outputs (`--flake`,
  `--output`), or of the attributes of a file (`--file`, `--attr`), with
  `nix derivation show -r`. `--runtime` reads the built store paths with
  `nix path-info -r` instead. It reads the nixpkgs revision that the tree
  pins in `flake.lock` or in `npins/sources.json`. `--nixpkgs-input` names
  the pin. It does not evaluate nixpkgs. It writes the full report to
  `maintainer-mode-scan.json`. This file stays on your machine.
- **`export`** downloads the public package list of `--server` from
  `GET /api/v1/packages/index`. `--index-file` reads a saved copy instead.
  Then it writes the strict report. Read it before you send it.
- **`submit`** does the same as `export`, with the list of `--to`. Then it
  sends the strict report to `POST /api/v1/reports`. It reads the upload
  token from `MAINTAINER_MODE_UPLOAD_TOKEN`, or from the file of
  `--token-file`.

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

The name of the scanned tree never leaves your machine. The client sends no
path, no store hash and no private name. The local report uses the nixfresh
report format, schema 1.

**The upload token.** Make it in `/me/settings#upload`. The server shows it once and
keeps only its SHA-256 hash. A new token replaces the old one. One button
revokes it.

**What the server does with a report.**

| Code | Meaning                                              |
| ---- | ---------------------------------------------------- |
| 202  | stored; the answer gives the kept and dropped counts |
| 400  | not a strict report; the error names the problem     |
| 401  | no token, or an unknown token                        |
| 413  | more than 16 MiB, or more than 50,000 packages       |
| 503  | the nixpkgs index is not built yet                   |

- It refuses a report with a field outside the strict schema, such as a
  tree name.
- It refuses a version that its index does not have for that attribute. The
  entry must then have `"version": null` and `"modified": true`.
- It refuses a pname that is not the pname of its index for that attribute.
- It drops an attribute that its index does not have, and counts it. The
  index can change between the download of the list and the upload. A
  broken or hostile client cannot store a private name.
- It keeps one set per login: a new report replaces the old one.
- It keeps the attributes, the time of the upload, the time of the scan and
  the nixpkgs revision. It keeps nothing else from the report.
- **Lifetime.** A set expires 90 days after its last upload. The background
  loop deletes it. "Delete my report" in the "On my machines" filter deletes it at once.
- **Access.** Only the owner sees the "On my machines" filter and the
  "on your machines" marks. There is no admin view. No public page, count or
  API uses an uploaded set.

**The list.** The "On my machines" filter of `/packages` shows the packages of your latest report and of your
scanned flakes. Each row has the health flags of a package page:
advisories, a newer upstream release, a failed Hydra build, open pull requests and open issues.
The rows that need attention come first. These packages also come first in Mine,
on the home page and in For you on `/review`.

**Alerts.** Your private Atom feed (see above) also gets, for each package in
your set, a new advisory and a new upstream release. It does not get the pull
requests and issues of the set, because a closure has thousands of packages.

**A public flake, scanned by the server.** This is off by default. When the
operator turns it on, the "On my machines" filter has a form for a public flake reference
(`github:OWNER/REPO`, `github:OWNER/REPO/BRANCH`, or an `https://` tarball)
and an output. After each nixpkgs index, the server scans the output again.
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

The tab **Ready to merge** of `/review` shows what waits for a person with merge rights. Everyone can see it. A committer who signs in also sees the section **In your areas** at the top. The home page of a committer links to the tab.

**Who is a committer.** Maintainer Mode counts a login as a committer when one of these is true:

- The login merged a pull request in the synced data in the last 365 days.
- The login is a member of the GitHub team `NixOS/nixpkgs-committers`. The server reads the team once a day with its own token. If the token cannot read the team, only the merges count.

`/api/v1/status` shows the counts. No page or API lists the logins.

**The sections.**

| Section                  | What it shows                                                                                                                                 |
| ------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------- |
| In your areas            | only for a committer who signed in: the ready-to-merge queue in your areas and in what you watch, 5 rows. Only you see it. Its id is `yours`. |
| Ready to merge           | the ready-to-merge queue: 20 rows on the first page, then 50 on each page. Authors with more review credit come first.                        |
| Approved by a maintainer | an approval on the current head by a login in the `meta.maintainers` or `meta.teams` of a package or module that the pull request changes     |
| Security fixes           | the label `1.severity: security`, and ready                                                                                                   |
| Backports                | a `release-*` base branch, and ready                                                                                                          |
| Waiting longest          | ready, oldest approval first                                                                                                                  |

In the last three sections, "ready" means that the checks pass, that there is no known conflict, and that the current head has an approval. The pull request does not need credit. Each row says why it is on the list. The last four sections put the longest wait first. They and **In your areas** show on the first page only, with 5 rows each. "see all" opens the full section, 50 rows on each page: `/review?tab=ready&section=<id>`. The ids are `maintainer_approved`, `security`, `backports` and `waiting`, as in the API.

Maintainer Mode never merges. Each row links to GitHub, and you decide. Read the [nixpkgs contributor guide](https://github.com/NixOS/nixpkgs/blob/master/CONTRIBUTING.md) for how to review and merge.

**The API.** `/api/v1/prs?queue=ready` gives the same lists without **In your areas**. It is the same for all callers, because your subscriptions are private.
