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
| `MAINTAINER_MODE_SYNC_TOKEN_FILE`      | optional token owned by the deployment                      |
| `GITHUB_APP_ID`                        | GitHub App whose installation token syncs first             |
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

## What we took, and under which licence

| Package                                                              | Version | Licence | Used for                                                                                            |
| -------------------------------------------------------------------- | ------- | ------- | --------------------------------------------------------------------------------------------------- |
| [`@primer/css`](https://www.npmjs.com/package/@primer/css)           | 22.3.2  | MIT     | `dist/primer.css`, served as-is                                                                     |
| [`@primer/octicons`](https://www.npmjs.com/package/@primer/octicons) | 19.40.0 | MIT     | thirteen 16px SVG icons                                                                             |
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
