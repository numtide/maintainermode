# Run it

```sh
nix build
GITHUB_TOKEN=… ./result/bin/maintainer-mode sync --db club.sqlite --index
./result/bin/maintainer-mode serve --db club.sqlite
```

| Variable                               | Meaning                                                     |
| -------------------------------------- | ----------------------------------------------------------- |
| `MAINTAINER_MODE_LISTEN`               | listen address, default `127.0.0.1:8790`                    |
| `MAINTAINER_MODE_METRICS_LISTEN`       | listen address of `/metrics`; off when not set              |
| `MAINTAINER_MODE_DB`                   | SQLite path                                                 |
| `MAINTAINER_MODE_PUBLIC_URL`           | the public URL; the OAuth callback is `<url>/auth/callback` |
| `GITHUB_CLIENT_ID`                     | OAuth app client id                                         |
| `GITHUB_CLIENT_SECRET_FILE`            | file holding the OAuth app client secret                    |
| `MAINTAINER_MODE_BOT_TOKEN_FILE`       | classic token of the bot account (scope `notifications`)    |
| `GITHUB_APP_ID`                        | GitHub App whose installation token reads GitHub first      |
| `GITHUB_APP_KEY_FILE`                  | file holding the PEM private key of the GitHub App          |
| `GITHUB_APP_INSTALLATION_ID`           | installation to use; else the one on the account of the App |
| `MAINTAINER_MODE_UPLOAD_TOKEN`         | the client's upload token for `submit`                      |
| `MAINTAINER_MODE_CHANNELS`             | channels next to master, default `nixos-unstable,stable`    |
| `MAINTAINER_MODE_S3_ENDPOINT`          | S3-compatible endpoint for the copy of the event log        |
| `MAINTAINER_MODE_S3_BUCKET`            | the bucket of that copy                                     |
| `MAINTAINER_MODE_S3_REGION`            | the region that the request signature names                 |
| `MAINTAINER_MODE_S3_CREDENTIALS_FILE`  | file with the access key id and the secret key              |
| `MAINTAINER_MODE_ARCHIVE_REPOS`        | repositories to archive; default `NixOS/nixpkgs`            |
| `MAINTAINER_MODE_ARCHIVE_ITEMS`        | items the archive walk fetches per repository and pass      |
| `MAINTAINER_MODE_ARCHIVE_GIT`          | `true` turns on the git copy of the archived repositories   |
| `MAINTAINER_MODE_GIT_DIR`              | where the git copy lives, default `git` next to the DB      |
| `MAINTAINER_MODE_RESCAN`               | `true` turns on the server rescans of public flakes         |
| `MAINTAINER_MODE_RESCAN_BWRAP`         | the `bwrap` program, default `bwrap`                        |
| `MAINTAINER_MODE_RESCAN_PRLIMIT`       | the `prlimit` program, default `prlimit`                    |
| `MAINTAINER_MODE_RESCAN_NIX`           | plain `nix` in `/nix/store`, default `nix` from `PATH`      |
| `MAINTAINER_MODE_RESCAN_MAX_FLAKES`    | flakes a person can register, default 3                     |
| `MAINTAINER_MODE_RESCAN_PER_DAY`       | scans a person gets in 24 hours, default 4                  |
| `MAINTAINER_MODE_RESCAN_TIMEOUT`       | wall-clock limit of one scan in seconds, default 900        |
| `MAINTAINER_MODE_RESCAN_MEMORY_MB`     | address-space limit of each sandboxed process, default 3072 |
| `MAINTAINER_MODE_RESCAN_PER_DAY_TOTAL` | scans in 24 hours for all people together, default 100      |

CI is the flake's `checks`: `nix flake check`.

## Metrics

The server serves Prometheus metrics in the text format 0.0.4. Set `--metrics-listen <address>` or `MAINTAINER_MODE_METRICS_LISTEN`. The server then opens a second listener that serves only `GET /metrics`. The main port answers 404 for `/metrics`.

In the NixOS module, set `services.maintainer-mode.metrics.listen`, for example to `"127.0.0.1:9431"`. The default is `null`, and the listener is off. `nix/instance.nix` sets `127.0.0.1:9431`.

Keep the listener on localhost. It has no authentication. Do not send it through the reverse proxy: there, every request comes from 127.0.0.1, so the server cannot tell the operator from a visitor.

A scrape does the work of one request to `/api/v1/sync`, and it reads two files in `/proc/self`.

| Metric                                                                 | Type      | Labels                      | Meaning                                                                     |
| ---------------------------------------------------------------------- | --------- | --------------------------- | --------------------------------------------------------------------------- |
| `maintainer_mode_build_info`                                           | gauge     | `version`, `commit`         | always 1; `commit` is `unknown` when the build does not know it             |
| `maintainer_mode_http_requests_total`                                  | counter   | `route`, `method`, `status` | requests to the main port; `route` is the route template or `unmatched`     |
| `maintainer_mode_http_request_duration_seconds`                        | histogram | `route`                     | time until the response head                                                |
| `maintainer_mode_http_signed_in_requests_total`                        | counter   |                             | requests whose handler found a valid session; no login                      |
| `maintainer_mode_process_resident_memory_bytes`                        | gauge     |                             | resident memory                                                             |
| `maintainer_mode_process_open_fds`                                     | gauge     |                             | open file descriptors                                                       |
| `maintainer_mode_process_start_time_seconds`                           | gauge     |                             | unix time of the start                                                      |
| `maintainer_mode_events_inserted_total`                                | counter   |                             | events added to the event log                                               |
| `maintainer_mode_events_last_insert_timestamp_seconds`                 | gauge     |                             | time of the last insert that added an event; 0 until the first one          |
| `maintainer_mode_recompute_duration_seconds`                           | gauge     |                             | wall time of the last finished recompute of the snapshot                    |
| `maintainer_mode_recompute_packages_duration_seconds`                  | gauge     |                             | the same for the package statistics                                         |
| `maintainer_mode_recompute_metrics_duration_seconds`                   | gauge     |                             | the same for the numbers of nixpkgs                                         |
| `maintainer_mode_recompute_total`, `_packages_total`, `_metrics_total` | counter   |                             | finished runs of each                                                       |
| `maintainer_mode_github_rate_remaining`                                | gauge     | `token`                     | GraphQL points left, from the last answer                                   |
| `maintainer_mode_github_rate_reset_timestamp_seconds`                  | gauge     | `token`                     | the time when that budget is full again                                     |
| `maintainer_mode_github_requests_total`                                | counter   | `kind`, `status`            | requests to the GitHub GraphQL API; `status` is `error` when no answer came |
| `maintainer_mode_sync_*`                                               | gauge     | see below                   | the data of `/api/v1/sync`                                                  |

`maintainer_mode_events_last_insert_timestamp_seconds` is the time of the insert, not the time of the event. A GH Archive backfill adds old events, so the `events.newest` of `/api/v1/sync` can be a day old while the log grows.

The `token` label is `app` for the installation token of the GitHub App and `bot` for the bot token. Both rate metrics come from the last answer to that token. After the reset time, the budget is full again, but the gauge keeps its value until the next answer. A lent token has no rate metric. `maintainer_mode_sync_tokens_lent` gives the number of lent tokens. `kind` is always `graphql`: the REST requests go out from many places, and the server does not count them.

### The `maintainer_mode_sync_*` gauges

Each scrape turns the JSON of `/api/v1/sync` into gauges. A new field in that JSON becomes a gauge, with no change to the metrics code.

- The name is `maintainer_mode_sync_` and the path of keys, joined with `_`. Each character other than `a-z`, `A-Z`, `0-9` and `_` becomes `_`. For example, `archive.git.bytes` becomes `maintainer_mode_sync_archive_git_bytes`.
- A number gives its value. `true` gives 1 and `false` gives 0.
- An RFC 3339 time gives the unix time, and the name ends in `_timestamp_seconds`. For example, `passes.last_ended` becomes `maintainer_mode_sync_passes_last_ended_timestamp_seconds`.
- Other strings, `null`, and arrays of numbers or strings give no gauge.
- Each object in an array gets a label from its first key among `repo`, `channel`, `source`, `name` and `id`. That key gives no gauge. An object with none of these keys gets the label `i`, its position in the array. For example, `archive[].items` becomes `maintainer_mode_sync_archive_items{repo="NixOS/nixpkgs"}`. When an outer array already uses the label, the inner label gets a number, such as `name2`.
- An object keyed by names stays in the path. For example, `health.sources.cve.at` becomes `maintainer_mode_sync_health_sources_cve_at_timestamp_seconds`.

## UX audit

`ux-audit` opens each page of the site in a headless Chromium. It takes full-page screenshots and checks each page for common faults. The page list is `nix/ux-pages.txt`. A unit test fails when a GET route is not in this list.

To audit a fresh local server, run:

```sh
nix run .#ux-audit -- --local
```

`--local` starts `maintainer-mode` on a temporary database and a free port. It blocks the outbound network of the server. It adds a test session, so it audits the pages signed out and signed in. The database is empty, so package and module pages answer 404.

Add `--seed` to fill the temporary database before the audit. The tool writes a small nixpkgs index, some upstream releases, a machine report and some watches for the test session. The signed-in lists of `/packages` and the home page then show rows.

To audit a running server, run:

```sh
nix run .#ux-audit -- --url https://maintainermode.org --only-public
```

To audit the signed-in pages of a running server, set `MAINTAINER_MODE_AUDIT_SESSION` to the value of your `session` cookie. The tool does not write or print this value.

The tool writes to `./ux-audit`. Use `--out` to change the folder. Open `index.html` in it. The page shows a table of issues, most severe first, and four screenshots per page: 390x844 and 1280x900, light and dark. `report.json` holds the same data.

| Option             | Meaning                                                                   |
| ------------------ | ------------------------------------------------------------------------- |
| `--pages <regex>`  | audit only the pages whose path matches                                   |
| `--only-public`    | skip the signed-in pages                                                  |
| `--set NAME=VALUE` | change a sample value of the page list, for example `--set pr=123`        |
| `--link-cap <n>`   | check at most n same-origin links, default 300                            |
| `--bin <path>`     | the binary for `--local`; in the dev shell, the default is `target/debug` |

The checks are: the HTTP status, the HTML size, the load time, console errors, failed requests, horizontal overflow, images without `alt`, form controls without a label, controls without a name, the order of the headings, duplicate ids, low text contrast, small tap targets on a phone, and same-origin links that answer 404. The link check does not open `/pr/`, `/auth/` and `/feed/` links.

## New secrets without a restart

The service reads each secret file again when the file changes. It checks each file at most once a minute. The log names the file that changed. The log never shows the secret.

systemd copies each secret file when the service starts (LoadCredential). The NixOS module adds a timer, `maintainer-mode-credentials.timer`. Each minute, the timer compares each secret file with its copy. When a file changed, the timer reloads `maintainer-mode.service`. The reload copies the secrets again (RefreshOnReload=credentials, systemd 260 or later). The service does not restart. With an older systemd, the service restarts.

A new secret takes effect within two minutes after the deploy. You do not have to restart the service. This works with sops-nix, clan vars and plain files.

To see when a secret changed, run `journalctl -u maintainer-mode-credentials -u maintainer-mode | grep changed`.

## What we took, and under which licence

| Package                                                              | Version | Licence | Used for                                                                             |
| -------------------------------------------------------------------- | ------- | ------- | ------------------------------------------------------------------------------------ |
| [`@primer/css`](https://www.npmjs.com/package/@primer/css)           | 22.3.2  | MIT     | `dist/primer.css`, trimmed to the classes in `src/` and to the light and dark themes |
| [`@primer/octicons`](https://www.npmjs.com/package/@primer/octicons) | 19.40.0 | MIT     | 16px SVG icons, sent as one hidden sprite on each page                               |

Both are fixed-output fetches of the npm tarballs, pinned by their sha512
(`nix/primer.nix`), and their licence files ship with the assets. The pages
are server-rendered HTML; there is no JavaScript.

### Page weight and caching

- `nix/primer.nix` trims `primer.css`. It keeps a rule only when each class in it is a word in `src/`. It drops all themes except light and dark. The dev shell uses the full file, so a new class works before the next build.
- The style sheets have the hash of their content in the name, for example `/static/primer.a023dd1c09a8.css`. The server sends them with `Cache-Control: public, max-age=31536000, immutable`.
- `icon()` writes `<svg><use href="#i-NAME"></use></svg>`. Each page holds one hidden sprite with the icons it uses.
- A public page has a weak `ETag`. The tag changes when the shared data changes, when the cookies change, and every five minutes. The five-minute step keeps the "N min ago" texts close to the truth. A request with the same tag gets `304 Not Modified`. `/sync`, `/pr/*`, `/p/*`, `/m/*` (they ask Hydra live), `/me`, `/inbox` and pages for a signed-in person have no `ETag`.
- The server compresses responses with gzip.

Version 22 of `@primer/css` no longer ships `Box`, `Label`, `UnderlineNav`,
`Subhead` or `flash`. `src/club.css` rebuilds those few on Primer's own
color tokens, so light and dark mode follow the bundle.

The hues are the NixOS brand palette from
[NixOS/branding](https://github.com/NixOS/branding) (`colors.toml`, OKLCH):
Afghani Blue for links, buttons and the header, Argentinian Blue for focus and
the current tab, and the accent colors for pass, pending, fail and done.
`src/club.css` sets them on Primer's tokens, and Primer keeps its grays. The
site uses the colors only, not the NixOS logo, and it is not an official
NixOS project.

Primer is GitHub's design system, used so that people who arrive from a pull
request page do not learn a second interface. The club is an independent
community project. It is not a GitHub product and is not endorsed by GitHub,
and it does not use GitHub's name or logo as its own.
