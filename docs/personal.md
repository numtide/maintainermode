# Your own view

What Maintainer Mode shows for you: what you maintain, what you watch, and
the packages of your own system.

## The menu

Every page has four tabs: Home, Review, Packages and nixpkgs. Your own pages are in a menu at the top right. Sign in, and the menu shows your login and the number of unread events. It has these items:

- **My packages** (`/me`): links to your inbox, what you maintain, what you watch and your machines.
- **Progress** (`/me/progress`).
- **Runs** (`/me/runs`): links to your workflow runs on `/actions`.
- **Settings** (`/me/settings`).
- **Sign out**.

Only you see the pages under `/me`. The server sends them with `Cache-Control: private, no-store`. `/u/<login>` is public.

## Get started

The home page shows a short introduction to visitors who are not signed in. It names the three ways to use Maintainer Mode. It links to the nixpkgs contributor guide (CONTRIBUTING.md). Press "Hide" to remove it. A cookie keeps it hidden for one year.

After your first sign-in, Maintainer Mode sends you to `/welcome`. This page is a checklist. Each step shows your real state: done, to do, or optional.

1. What you maintain. Subscribe to all of it in one step.
2. What you watch. Subscribe to a package, a module or a directory.
3. The review queue for you, on `/review`.
4. Notifications. Make your private Atom feed on `/watch`.
5. Your system. Upload your closure on `/me/sets`.
6. Actions. Save your workflow repositories on `/actions`.
7. LLMs. They are off by default. Turn them on in `/me/settings`.
8. The nixpkgs guides: CONTRIBUTING.md, pkgs/README.md, nixos/README.md, maintainers/README.md and the Nixpkgs manual.

If you maintain nothing, the first step changes. It sends you to the pull requests with no maintainer on `/review`, with a checklist for a first review. It also links maintainers/README.md.

Your menu shows "Get started" until you press "I am done". You can open `/welcome` again at any time. Only you see it.

## What I maintain

`/u/<login>/maintains` lists the packages and NixOS modules whose
`meta.maintainers` names the login, or whose `meta.teams` names a team the
login is in. Each row carries the same health flags as a page. The highest
[risk score](health.md#the-risk-score) comes first. The list comes from nixpkgs;
change it there.

## What I watch

Subscribe to any package, module or area from its page, by name on `/watch`,
or to everything you maintain in one step. `/watch` shows each subscription
with its flags, the ones that need attention first.

- **The Atom feed** is the delivery. `/watch` makes a private link,
  `/feed/<token>`, with the last 30 days of what happened to your
  subscriptions: pull requests opened, merged or closed, issues opened or
  closed, and advisories, when the tracker opens their nixpkgs issue. An
  advisory with no nixpkgs issue shows on the page, not in the feed.
  Replacing the link cuts the old one.
- Subscriptions are private: they show only to you and in your feed. The
  package pages are public.
- **Why here.** Accounts, stored preferences and outbound messages are a write
  path. Maintainer Mode already logs people in and keeps state, so a
  subscription is one more row in its database.

## Review, for you

`/review` is the one place to review. It has five tabs, and each tab shows its count:

| Tab            | Address               | What it lists                                                             |
| -------------- | --------------------- | ------------------------------------------------------------------------- |
| For you        | `/review?tab=foryou`  | pull requests that touch what you maintain, own, changed, tagged or watch |
| No maintainer  | `/review?tab=unowned` | pull requests with no maintainer, team or `ci/OWNERS` line on their files |
| Bumps          | `/review?tab=bumps`   | open version bumps (see [health.md](health.md))                           |
| All            | `/review?tab=all`     | every pull request in the queues that is not ready to merge               |
| Ready to merge | `/review?tab=ready`   | the ready-to-merge queue, and the lists for committers (see below)        |

Sign in, and `/review` opens on **For you**. For you looks at `meta.maintainers`, `meta.teams`, `ci/OWNERS`, the paths you changed this year, your tags, and every package, module or directory you watch. Signed out, `/review` opens on **No maintainer**, and For you is not shown.

Each tab shows 50 rows. "Next page" adds `?after=<number>`. On most tabs, it is the number of the last pull request on the page. On Ready to merge, it is the count of rows on the previous pages, because that tab is not in number order. `/merge`, `/committer` and `/bumps` redirect to their tab.

The JSON API takes `GET /api/v1/queue/review?bucket=foryou&login=<login>`. The MCP tool `queue_review` and the CLI (`maintainer-mode review --bucket foryou --login <login>`) take the same name. `mine` is an alias of `foryou`. Your subscriptions are private. They count only when your own session or your own MCP token asks for your login. In all other cases only your public area counts.

## Your progress

`/me/progress` shows your own review record. Only you see it. It never shows the numbers of another person. There is no leaderboard.

- **This week and this month**: the pull requests you reviewed, and your reviews that earned credit. A week starts on Monday, in UTC.
- **Streak**: the weeks in a row with at least one review that earned credit. A week with no credit yet does not break the streak until the week ends.
- **Weekly goal**: you set the number of pull requests to review each week, from 1 to 50. Set 0 to remove the goal. `/review` shows how far you are.
- **Badges**: first review; first review that earned credit on an unowned pull request; first review merged unchanged; 10, 50 and 100 reviews; reviews in 5 areas of the state of nixpkgs.
- **Being listed**: how many more authors you need. You need credit from 3 different authors in 60 days.

The page counts the synced data only: the last 60 days and every open pull request. A small box on `/review` shows your week, your streak and your goal. Your menu and your own `/u/<login>` page link to the page.

## Notifications

**The inbox.** `/me/inbox` shows the events of your Atom feed: what
happened in the last 30 days to what you watch and to your package set.
The newest event comes first. Filter the list by kind (pull request,
issue, advisory, release) or by subscription. "Mark all read" marks
every event as read. Only you see your inbox.

**The unread count.** The header shows the number of unread events. The
server counts them after each sync pass, and when you open the inbox.
On `/me/settings`, choose the kinds that the count includes.

**The email digest.** This is off by default. It works only when the
operator sets up SMTP (see [operations.md](operations.md#email)). Enter
an address on `/me/settings` to get the digest. The server sends at most
one email a day. The email lists the unread events of your chosen kinds
since the last digest. "Send me a test email" sends one email, at most
once in 10 minutes.

- The address is private. It shows only to you, on `/me/settings`.
- The server accepts only a plain address, like `name@example.org`. It
  refuses spaces, names and quotes.
- Each email has an unsubscribe link with a new random token. The server
  keeps only the hash of the token, for 60 days. The link opens a page
  with one button. Mail clients that know RFC 8058 unsubscribe in one
  click. Unsubscribing deletes the address.
- To stop the digest, you can also empty the address on `/me/settings`.

### Confirm your address

### Confirm your address

The server sends no digest to an address that you did not confirm.
When you enter or change the address on /me/settings, the server stores it as pending.
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
evaluates it. Three client commands do the work:

```sh
maintainer-mode scan --flake . --output nixosConfigurations.<host>.config.system.build.toplevel
maintainer-mode export --out report.json
maintainer-mode submit --to https://<host> --token-file ~/.config/maintainer-mode/upload-token
```

- **`scan`** reads the closure of one or more flake outputs (`--flake`,
  `--output`), or of the attributes of a file (`--file`, `--attr`), with
  `nix derivation show -r`. `--runtime` reads the built store paths with
  `nix path-info -r` instead. It matches each package name to a nixpkgs
  attribute: the name, the name in lower case, or `python3Packages.<name>`.
  It uses the nixpkgs that the tree pins in `flake.lock` or in
  `npins/sources.json`. `--nixpkgs-input` names the pin, and `--nixpkgs` gives
  a path or a flake reference. It writes the full report to
  `maintainer-mode-scan.json`, which stays on your machine.
- **`export`** writes the redacted report. It keeps only the packages with a
  nixpkgs attribute. It counts the other names and does not list them. It
  removes the name of the scanned tree. Read it before you send it.
- **`submit`** sends the redacted report to `POST /api/v1/reports`. It reads
  the upload token from `MAINTAINER_MODE_UPLOAD_TOKEN`, or from the file of
  `--token-file`.

The report format is the nixfresh report format, schema 1.

**The upload token.** Make it on `/me/sets`. The server shows it once and
keeps only its SHA-256 hash. A new token replaces the old one. One button
revokes it.

**What the server does with a report.**

| Code | Meaning                                              |
| ---- | ---------------------------------------------------- |
| 202  | stored; the answer gives the kept and dropped counts |
| 400  | an unreadable report or a schema other than 1        |
| 401  | no token, or an unknown token                        |
| 413  | more than 16 MiB, or more than 50,000 packages       |
| 503  | the nixpkgs index is not built yet                   |

- It keeps an attribute only when its own nixpkgs index has it. It drops all
  other entries. A broken or hostile client cannot store a private name.
- It keeps one set per login: a new report replaces the old one.
- It keeps the attributes, the time of the upload, the time of the scan and
  the nixpkgs revision. It keeps nothing else from the report.
- **Lifetime.** A set expires 90 days after its last upload. The background
  loop deletes it. "Delete my set" on `/me/sets` deletes it at once.
- **Access.** Only the owner sees `/me/sets`. There is no admin view. No
  public page, count or API uses an uploaded set.

**The panel.** `/me/sets` shows the attributes of your latest report and the
packages you watch. Each row has the health flags of a package page:
advisories, a newer upstream release, open pull requests and open issues.
The rows that need attention come first. The package page has the last Hydra
build.

**Alerts.** Your private Atom feed (see above) also gets, for each package in
your set, a new advisory and a new upstream release. It does not get the pull
requests and issues of the set, because a closure has thousands of packages.

**A public flake, scanned by the server.** This is off by default. When the
operator turns it on, `/me/sets` has a form for a public flake reference
(`github:OWNER/REPO`, `github:OWNER/REPO/BRANCH`, or an `https://` tarball)
and an output. After each nixpkgs index, the server scans the output again.
It keeps the result with the rules of an upload: the same redaction, the
same index filter and the same 90-day rule. The server stops the scans of a
person who has not opened `/me/sets` for 90 days. The rows of the panel then
show "in a scanned flake".

- The scan evaluates third-party code. Each nix step runs in a bubblewrap
  sandbox, as a separate process, with a read-only store, its own temporary
  directory, no service state, no secret, and a memory and time limit.
- The evaluation step has no network. The two fetch steps have the network
  of the host, and fetch only the inputs that a checked `flake.lock` names.
- Nix runs with `allow-import-from-derivation = false`: nothing is built.
- A person has at most 3 flakes and 4 scans a day, by default.
- `/sync` shows the flakes, the queue, the last scan and the failures, as
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

| Section                  | What it shows                                                                                                                             |
| ------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------- |
| In your areas            | only for a committer who signed in: the ready-to-merge queue in your areas and in what you watch. Only you see it.                        |
| Ready to merge           | the ready-to-merge queue, 50 rows on each page. Authors with more review credit come first.                                               |
| Approved by a maintainer | an approval on the current head by a login in the `meta.maintainers` or `meta.teams` of a package or module that the pull request changes |
| Security fixes           | the label `1.severity: security`, and ready                                                                                               |
| Backports                | a `release-*` base branch, and ready                                                                                                      |
| Waiting longest          | ready, oldest approval first                                                                                                              |

In the last three sections, "ready" means that the checks pass, that there is no known conflict, and that the current head has an approval. The pull request does not need credit. Each row says why it is on the list. The last four sections put the longest wait first and show 50 rows at most. They show on the first page only.

Maintainer Mode never merges. Each row links to GitHub, and you decide. Read the [nixpkgs contributor guide](https://github.com/NixOS/nixpkgs/blob/master/CONTRIBUTING.md) for how to review and merge.

**The API.** `/api/v1/committer` gives the same lists without **In your areas**. It is the same for all callers, because your subscriptions are private.
