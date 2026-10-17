# Run it

```sh
nix build
GITHUB_TOKEN=… ./result/bin/maintainer-mode sync --db club.sqlite --index
./result/bin/maintainer-mode serve --db club.sqlite
```

| Variable                               | Meaning                                                     |
| -------------------------------------- | ----------------------------------------------------------- |
| `MAINTAINER_MODE_LISTEN`               | listen address, default `127.0.0.1:8790`                    |
| `MAINTAINER_MODE_DB`                   | SQLite path                                                 |
| `MAINTAINER_MODE_PUBLIC_URL`           | the public URL; the OAuth callback is `<url>/auth/callback` |
| `GITHUB_CLIENT_ID`                     | OAuth app client id                                         |
| `GITHUB_CLIENT_SECRET_FILE`            | file holding the OAuth app client secret                    |
| `MAINTAINER_MODE_BOT_TOKEN_FILE`       | classic token of the bot account (scope `notifications`)    |
| `GITHUB_APP_ID`                        | GitHub App whose installation token syncs first             |
| `GITHUB_APP_KEY_FILE`                  | file holding the PEM private key of the GitHub App          |
| `GITHUB_APP_INSTALLATION_ID`           | installation to use; else the one on the account of the App |
| `MAINTAINER_MODE_GITHUB_APP_SLUG`      | the App people install on their workflow repository         |
| `MAINTAINER_MODE_UPLOAD_TOKEN`         | the client's upload token for `submit`                      |
| `MAINTAINER_MODE_CHANNELS`             | channels next to master, default `nixos-unstable,stable`    |
| `MAINTAINER_MODE_S3_ENDPOINT`          | S3-compatible endpoint for the copy of the event log        |
| `MAINTAINER_MODE_S3_BUCKET`            | the bucket of that copy                                     |
| `MAINTAINER_MODE_S3_REGION`            | the region that the request signature names                 |
| `MAINTAINER_MODE_S3_CREDENTIALS_FILE`  | file with the access key id and the secret key              |
| `MAINTAINER_MODE_ARCHIVE_REPOS`        | repositories to archive; default `NixOS/nixpkgs`            |
| `MAINTAINER_MODE_ARCHIVE_ITEMS`        | items the archive walk fetches per repository and pass      |
| `MAINTAINER_MODE_RESCAN`               | `true` turns on the server rescans of public flakes         |
| `MAINTAINER_MODE_RESCAN_BWRAP`         | the `bwrap` program, default `bwrap`                        |
| `MAINTAINER_MODE_RESCAN_PRLIMIT`       | the `prlimit` program, default `prlimit`                    |
| `MAINTAINER_MODE_RESCAN_NIX`           | plain `nix` in `/nix/store`, default `nix` from `PATH`      |
| `MAINTAINER_MODE_RESCAN_MAX_FLAKES`    | flakes a person can register, default 3                     |
| `MAINTAINER_MODE_RESCAN_PER_DAY`       | scans a person gets in 24 hours, default 4                  |
| `MAINTAINER_MODE_RESCAN_TIMEOUT`       | wall-clock limit of one scan in seconds, default 900        |
| `MAINTAINER_MODE_RESCAN_MEMORY_MB`     | address-space limit of each sandboxed process, default 3072 |
| `MAINTAINER_MODE_RESCAN_PER_DAY_TOTAL` | scans in 24 hours for all people together, default 100      |
| `MAINTAINER_MODE_SMTP_HOST`            | SMTP server for the email digest; off without it            |
| `MAINTAINER_MODE_SMTP_PORT`            | SMTP port, default 465 (TLS); another port needs STARTTLS   |
| `MAINTAINER_MODE_SMTP_USER`            | SMTP user, with the password file                           |
| `MAINTAINER_MODE_SMTP_PASSWORD_FILE`   | file holding the SMTP password                              |
| `MAINTAINER_MODE_SMTP_FROM`            | the From address of the digest                              |
| `MAINTAINER_MODE_MAIL_PER_DAY`         | emails the server sends in 24 hours, default 500            |

CI is the flake's `checks`: `nix flake check`.

## Email

Set `MAINTAINER_MODE_SMTP_HOST` and `MAINTAINER_MODE_SMTP_FROM` to turn
the email digest on. `MAINTAINER_MODE_SMTP_PORT` is 465 by default. Port
465 uses TLS from the start. Any other port must offer STARTTLS. Set
`MAINTAINER_MODE_SMTP_USER` and `MAINTAINER_MODE_SMTP_PASSWORD_FILE`
together, or leave both unset. In the NixOS module, use
`services.maintainer-mode.smtp`. The password file goes through
LoadCredential.

## Email limits

The server sends at most 500 emails in 24 hours: digests, test emails and confirmations together.
To change the limit, set `MAINTAINER_MODE_MAIL_PER_DAY` or the module option `services.maintainer-mode.smtp.perDay`.
Each person gets at most 3 confirmation emails and 3 test emails in 24 hours.
Over a limit, the server does not send the email and writes a warning to the log.
The Data status page (`/state?tab=status`) shows the emails sent and skipped in the last 24 hours. It shows no address and no login.

## New secrets without a restart

The service reads each secret file again when the file changes. It checks each file at most once a minute. The log names the file that changed. The log never shows the secret.

systemd copies each secret file when the service starts (LoadCredential). The NixOS module adds a timer, `maintainer-mode-credentials.timer`. Each minute, the timer compares each secret file with its copy. When a file changed, the timer reloads `maintainer-mode.service`. The reload copies the secrets again (RefreshOnReload=credentials, systemd 260 or later). The service does not restart. With an older systemd, the service restarts.

A new secret takes effect within two minutes after the deploy. You do not have to restart the service. This works with sops-nix, clan vars and plain files.

To see when a secret changed, run `journalctl -u maintainer-mode-credentials -u maintainer-mode | grep changed`.

## What we took, and under which licence

| Package                                                              | Version | Licence | Used for                                                                                            |
| -------------------------------------------------------------------- | ------- | ------- | --------------------------------------------------------------------------------------------------- |
| [`@primer/css`](https://www.npmjs.com/package/@primer/css)           | 22.3.2  | MIT     | `dist/primer.css`, trimmed to the classes in `src/` and to the light and dark themes                |
| [`@primer/octicons`](https://www.npmjs.com/package/@primer/octicons) | 19.40.0 | MIT     | 16px SVG icons, sent as one hidden sprite on each page                                              |
| nixfresh `nix.rs` and `attrs.rs`                                     | 2026-10 | MIT     | `src/eval.rs` and `src/attrs.rs`: the evaluator wrapper and the attribute index, copied and trimmed |
| nixfresh repology and tracker collectors                             | 2026-10 | MIT     | `src/health.rs`: the bulk reads of repology and the security tracker, rewritten for one host        |
| nixfresh `upstream.rs`, `version.rs` and direct collector            | 2026-10 | MIT     | `src/upstream.rs` and `src/version.rs`: upstream releases from tags and registries                  |
| nixfresh `score.rs`                                                  | 2026-10 | MIT     | `src/score.rs`: the risk score, simplified                                                          |
| nixfresh osv collector                                               | 2026-10 | MIT     | `src/osv.rs`: OSV matches for the exact version of PyPI, crates.io and npm packages                 |
| nixfresh cve, nvd and epss collectors                                | 2026-10 | MIT     | `src/severity.rs`: CVSS, CISA exploitation and EPSS for the tracker's CVEs                          |
| nixfresh `nix.rs` and `report.rs`                                    | 2026-10 | MIT     | `src/scan.rs` and `src/report.rs`: the closure scan, the report format and its redaction, trimmed   |

Both are fixed-output fetches of the npm tarballs, pinned by their sha512
(`nix/primer.nix`), and their licence files ship with the assets. The pages
are server-rendered HTML; there is no JavaScript.

### Page weight and caching

- `nix/primer.nix` trims `primer.css`. It keeps a rule only when each class in it is a word in `src/`. It drops all themes except light and dark. The dev shell uses the full file, so a new class works before the next build.
- The style sheets have the hash of their content in the name, for example `/static/primer.a023dd1c09a8.css`. The server sends them with `Cache-Control: public, max-age=31536000, immutable`.
- `icon()` writes `<svg><use href="#i-NAME"></use></svg>`. Each page holds one hidden sprite with the icons it uses.
- A public page has a weak `ETag`. The tag changes when the shared data changes, when the cookies change, and every five minutes. The five-minute step keeps the "N min ago" texts close to the truth. A request with the same tag gets `304 Not Modified`. `/sync`, `/pr/*`, `/p/*`, `/m/*` (they ask Hydra live), `/me` and pages for a signed-in person have no `ETag`.
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
