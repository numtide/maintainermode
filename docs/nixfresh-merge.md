# Plan: merge nixfresh into Maintainer Mode

Status: proposed. No code moves until this plan is approved.

## Why

Both tools sweep nixpkgs, read the same pull requests and run on the same
host. Maintainer Mode already copies nixfresh's evaluator (`src/eval.rs`) and
attribute sweep (`src/attrs.rs`). One binary, one database and one sweep cost
less to run and to maintain.

## The line

Facts about nixpkgs are public. Facts about a person's machines are private.

| Public, on every page                       | Private, owner only                    |
| ------------------------------------------- | -------------------------------------- |
| queues, review record, the state of nixpkgs | uploaded closures                      |
| advisories, upstream lag, maintenance gaps  | alerts that follow from those closures |
| maintainer practice                         |                                        |
| counts over public trees only               |                                        |

A number on a public page never uses an uploaded closure.

## What moves

| From nixfresh                                         | Becomes                                           |
| ----------------------------------------------------- | ------------------------------------------------- |
| `index` (attribute sweep)                             | the one sweep; Maintainer Mode's copy goes away   |
| collectors: tracker, cve, epss, osv, repology, direct | the daily refresh                                 |
| `practice`, `maintainers`                             | maintainer facts on package and person pages      |
| `github` collector                                    | removed: the sync already has these PRs           |
| `judge`                                               | stays out: it sends package data to a third party |
| dashboard, risk table                                 | package pages and `/state`                        |
| `scan`, `export`, `submit`                            | the open source client, run by the person         |
| `/api/v1/reports` with a token file                   | the private panel, behind login                   |
| `/api/v1/triage`, `/refresh`, `/scan`                 | removed from the server                           |

`nixfresh-mend` stays a separate binary. It writes pull requests, and this
service never writes to GitHub.

## The private panel

A person with an account subscribes to a package set. Alerts follow from it.
The set comes from one or more of these:

- **Watched packages.** The "packages I watch" list that exists today.
- **An uploaded report.** From `submit` or the browser.
- **A public flake reference.** The server scans it again after each nixpkgs
  sweep. This evaluates third-party code, so it runs in the nixfresh
  bubblewrap sandbox, with a limit per account.

The set is private, even when every package in it is public: the choice shows
what the person runs.

- **Content.** The server accepts the nixfresh report format and drops every
  package without a nixpkgs attribute, as the nixfresh scanner does today
  (nixfresh `docs/privacy.md`).
- **Access.** Only the owner reads it. There is no admin view.
- **Lifetime.** One action deletes the set. An uploaded report expires after
  90 days without a new upload.
- **Purpose.** Alerts: a new advisory, a fix that lands, a package that loses
  its last practising maintainer.
- **Notice.** The panel states what the server stores and why.

The alert channel is open. See below.

## Order

1. Finish the nixpkgs1 deploy as it is.
2. Publish nixfresh (owner and licence are open).
3. Move the sweep and the collectors. Delete the copied evaluator.
4. Add package facts to the package pages and `/state`.
5. Add the private panel and alerts.
6. Move `scan`, `export` and `submit` to the client.

Steps 5 and 6 are done: `/me/sets`, the upload token, the alerts in the Atom
feed, and the three client commands. The public flake reference is not done.

## Open

- Alert channel: email needs the GitHub App's email permission and a mail
  sender; an Atom feed per account needs neither.
- Owner and licence of the published nixfresh code.
- Retention period: 90 days is a proposal.
- Scan limits for flake references: how many per account, and how often.
