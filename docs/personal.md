# Your own view

What Maintainer Mode shows for you: what you maintain, what you watch, and
the packages of your own system.

## The menu

Every page has four tabs: Home, Review, Packages and nixpkgs. Your own pages are in a menu at the top right. Sign in, and the menu shows your login and the number of unread events. It has these items:

- **My packages** (`/me`), with four tabs: Inbox, Maintain, Watch and My machines.
- **Progress** (`/me/progress`).
- **Runs** (`/me/runs`): your workflow runs.
- **Settings** (`/me/settings`).
- **Sign out**.

Only you see the pages under `/me`. The server sends every answer under `/me` with `Cache-Control: private, no-store`. `/u/<login>` is public, and it is the same for every viewer.

| Tab or page        | What it shows                                                       |
| ------------------ | ------------------------------------------------------------------- |
| `/me?tab=inbox`    | the events of what you watch and of your machines (the default tab) |
| `/me?tab=maintain` | what nixpkgs names you as a maintainer of, and "Watch all of them"  |
| `/me?tab=watch`    | what you watch, and a form to watch more                            |
| `/me?tab=machines` | the packages of your machines                                       |
| `/me/runs`         | your nixpkgs-review and nix-update runs                             |
| `/me/settings`     | every choice and token, in one page                                 |

The old addresses `/watch`, `/me/inbox` and `/me/sets` redirect to their tab.
Only you see the pages under `/me`. The server sends them with `Cache-Control: private, no-store`. `/u/<login>` is public and never shows private data. It has two tabs: **Record** and **Maintains**. Search for `@login` on `/packages` to open it.

## Settings

`/me/settings` is one page with these sections. The links at the top go to each section.

| Section      | What you set                                                                                   |
| ------------ | ---------------------------------------------------------------------------------------------- |
| `#email`     | the kinds of events that the unread count and the email digest show, and the digest address    |
| `#feed`      | the private link of your Atom feed                                                             |
| `#upload`    | the upload token for `maintainer-mode submit`                                                  |
| `#llm`       | the LLM choice (see [actions.md](actions.md#llms-if-you-want-them))                            |
| `#mcp`       | the MCP token                                                                                  |
| `#workflows` | the setup checklist and the repositories where "Review in CI" and "Update with nix-update" run |
| `#tags`      | the path prefixes that the For you tab of `/review` adds                                       |
| `#goal`      | your weekly goal                                                                               |
| `#start`     | show or hide the Get started card                                                              |

## Get started

The home page shows a short introduction to visitors who are not signed in. It names the three ways to use Maintainer Mode. It links to the nixpkgs contributor guide (CONTRIBUTING.md). Press "Hide" to remove it. A cookie keeps it hidden for one year.

After you sign in, the home page shows the Get started card. It is a checklist. Each step shows your real state: done, to do, or optional.

1. What you maintain. Watch all of it in one step.
2. What you watch. Watch a package, a module or a directory.
3. The review queue for you, on `/review`.
4. Notifications. Make your private Atom feed in `/me/settings#feed`.
5. Your system. Upload your closure, and see it in the My machines tab.
6. Actions. Follow the setup checklist in `/me/settings#workflows`.
7. LLMs. They are off by default. Turn them on in `/me/settings#llm`.
8. The nixpkgs guides: CONTRIBUTING.md, pkgs/README.md, nixos/README.md, maintainers/README.md and the Nixpkgs manual.

If you maintain nothing, the first step changes. It sends you to the pull requests with no maintainer on `/review`, with a checklist for a first review. It also links maintainers/README.md.

Press "Hide" on the card to remove it. Your menu shows "Get started" until you do. To show the card again, use `/me/settings#start`. Only you see the card.

## What I maintain

The Maintain tab of `/me` lists the packages and NixOS modules whose
`meta.maintainers` names you, or whose `meta.teams` names a team you are
in. Each row carries the same health flags as a page. The highest
[risk score](health.md#the-risk-score) comes first. "Watch all of them" adds
every row to what you watch. The list comes from nixpkgs; change it there.
The **Maintains** tab of `/u/<login>` lists the packages and NixOS modules whose
`meta.maintainers` names the login, or whose `meta.teams` names a team the
login is in. Each row carries the same health flags as a page. The highest
[risk score](health.md#the-risk-score) comes first. The list comes from nixpkgs;
change it there.

`/u/<login>/maintains` shows the same list for any login. It is public.

## What I watch

Watch any package, module or area from its page, by name on the Watch tab of
`/me`, or everything you maintain in one step on the Maintain tab. The Watch
tab shows each item with its flags, the ones that need attention first.

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

| Tab            | Address               | What it lists                                                             |
| -------------- | --------------------- | ------------------------------------------------------------------------- |
| For you        | `/review?tab=foryou`  | pull requests that touch what you maintain, own, changed, tagged or watch |
| No maintainer  | `/review?tab=unowned` | pull requests with no maintainer, team or `ci/OWNERS` line on their files |
| Bumps          | `/review?tab=bumps`   | open version bumps (see [health.md](health.md))                           |
| All            | `/review?tab=all`     | every pull request in the queues that is not ready to merge               |
| Ready to merge | `/review?tab=ready`   | the ready-to-merge queue, and the lists for committers (see below)        |

Sign in, and `/review` opens on **For you**. For you looks at `meta.maintainers`, `meta.teams`, `ci/OWNERS`, the paths you changed this year, your tags, and every package, module or directory you watch. Signed out, `/review` opens on **No maintainer**, and For you is not shown.

Each tab shows 50 rows. "Next page" adds `?after=<number>`. On most tabs, it is the number of the last pull request on the page. On Ready to merge, it is the count of rows on the previous pages, because that tab is not in number order. `/merge`, `/committer` and `/bumps` redirect to their tab.

The JSON API takes `GET /api/v1/prs?queue=foryou&login=<login>`. The MCP tool `prs` and the CLI (`maintainer-mode prs --queue foryou --login <login>`) take the same names. Your subscriptions are private. They count only when your own session or your own MCP token asks for your login. In all other cases only your public area counts. See [api.md](api.md).

## Your progress

`/me/progress` shows your own review record. Only you see it. It never shows the numbers of another person. There is no leaderboard.

- **This week and this month**: the pull requests you reviewed, and your reviews that earned credit. A week starts on Monday, in UTC.
- **Streak**: the weeks in a row with at least one review that earned credit. A week with no credit yet does not break the streak until the week ends.
- **Weekly goal**: you set the number of pull requests to review each week, from 1 to 50. Set 0 to remove the goal. `/review` shows how far you are.
- **Badges**: First review; First review with no maintainer; Merged unchanged; 10, 50 and 100 reviews; Five parts of nixpkgs.
- **In the queues**: how many more authors you need before your pull requests show in the queues. You need credit from 3 different authors in 60 days.

The page counts the synced data only: the last 60 days and every open pull request. A small box on `/review` shows your week, your streak and your goal. Your menu links to the page.

## Notifications

**The inbox.** The Inbox tab of `/me` shows the events of your Atom feed: what
happened in the last 30 days to what you watch and to your package set.
The newest event comes first. Filter the list by kind (pull request,
issue, advisory, release) or by subscription. "Mark all read" marks
every event as read. Only you see your inbox.

**The unread count.** The header shows the number of unread events. The
server counts them after each sync pass, and when you open the inbox.
In `/me/settings#email`, choose the kinds that the count includes.

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
evaluates it. Three client commands do the work:

```sh
maintainer-mode scan --flake . --output nixosConfigurations.<host>.config.system.build.toplevel
maintainer-mode --server https://<host> export --out report.json
maintainer-mode submit --to https://<host> --token-file ~/.config/maintainer-mode/upload-token
```

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
  loop deletes it. "Delete my set" on the My machines tab deletes it at once.
- **Access.** Only the owner sees the My machines tab. There is no admin view. No
  public page, count or API uses an uploaded set.

**The panel.** The My machines tab of `/me` shows the attributes of your latest report and the
packages you watch. Each row has the health flags of a package page:
advisories, a newer upstream release, open pull requests and open issues.
The rows that need attention come first. The package page has the last Hydra
build.

**Alerts.** Your private Atom feed (see above) also gets, for each package in
your set, a new advisory and a new upstream release. It does not get the pull
requests and issues of the set, because a closure has thousands of packages.

**A public flake, scanned by the server.** This is off by default. When the
operator turns it on, the My machines tab has a form for a public flake reference
(`github:OWNER/REPO`, `github:OWNER/REPO/BRANCH`, or an `https://` tarball)
and an output. After each nixpkgs index, the server scans the output again.
It keeps the result with the rules of an upload: the same public-list filter, the
same index filter and the same 90-day rule. The server stops the scans of a
person who has not opened the My machines tab for 90 days. The rows of the panel then
show "in a scanned flake".

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

| Section                  | What it shows                                                                                                                             |
| ------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------- |
| In your areas            | only for a committer who signed in: the ready-to-merge queue in your areas and in what you watch. Only you see it.                        |
| Ready to merge           | the ready-to-merge queue, 50 rows on each page. Authors with more review credit come first.                                               |
| Approved by a maintainer | an approval on the current head by a login in the `meta.maintainers` or `meta.teams` of a package or module that the pull request changes |
| Security fixes           | the label `1.severity: security`, and ready                                                                                               |
| Backports                | a `release-*` base branch, and ready                                                                                                      |
| Waiting longest          | ready, oldest approval first                                                                                                              |

In the last three sections, "ready" means that the checks pass, that there is no known conflict, and that the current head has an approval. The pull request does not need credit. Each row says why it is on the list. The last four sections put the longest wait first. They show on the first page only, with 5 rows each. "see all" opens the full section, 50 rows on each page: `/review?tab=ready&section=<id>`. The ids are `maintainer_approved`, `security`, `backports` and `waiting`, as in the API.

Maintainer Mode never merges. Each row links to GitHub, and you decide. Read the [nixpkgs contributor guide](https://github.com/NixOS/nixpkgs/blob/master/CONTRIBUTING.md) for how to review and merge.

**The API.** `/api/v1/prs?queue=ready` gives the same lists without **In your areas**. It is the same for all callers, because your subscriptions are private.
