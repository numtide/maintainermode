# Actions and LLMs

The buttons that start work on your behalf, in your own repositories.

## Actions

Two buttons start work in GitHub Actions of a repository you own, with your
token, when you press one. Nothing runs on the Maintainer Mode host. Signed out, `/actions` says what the buttons do. Signed in, it opens `/me/runs`.

- **Review in CI**, on any pull request row or by number on `/me/runs`, starts `review.yml`
  of your fork of
  [nixpkgs-review-gha](https://github.com/Defelo/nixpkgs-review-gha) with the
  pull request number. Its own defaults decide the rest, like posting the
  result.
- **Update with nix-update**, on a package page, starts `update.yml` with the
  attribute and the version (repology's newest release, which you can change).
  It is a small re-implementation of nixpkgs-update on
  [nix-update](https://github.com/Mic92/nix-update): clone nixpkgs, run
  nix-update (the package's `updateScript` when it has one and no version is
  given), build the package, and push `update/<attr>` to your nixpkgs fork.
  When nix-update changes something, the workflow pushes the branch. **It pushes the branch whether the build passes or not**, so a failed update is a branch to keep working on. When nix-update changes nothing, the package is already up to date. The run then passes, its summary says "already up to date", and the workflow pushes no branch.
  It never opens a pull request: the run and
  `/me/runs` link the page where you open one. A second job pushes the branch.
  Only that job gets `GH_TOKEN`, and it runs no code from nixpkgs or upstream.
- `/me/runs` lists your recent runs of both workflows. For the last run of each attribute, it reads the jobs of the run and shows one of these results:
  - **Open a pull request**: the update and the build passed, and the branch is on your fork.
  - **See the branch, keep working**: the branch is on your fork, but the update or the build failed.
  - **already up to date**: nix-update changed nothing, so the workflow pushed no branch.
  - **no branch was pushed**: the run failed before it changed anything.

To set it up:

1. Fork nixpkgs-review-gha, and copy [`gha/update.yml`](../gha/update.yml) to its
   `.github/workflows/`. It calls the reusable workflow in
   `.github/workflows/update.yml` of this repository.
2. Give it a `GH_TOKEN` secret that can push to your fork of nixpkgs.
3. Install the Maintainer Mode GitHub App on that repository. The App needs
   the "Actions: read and write" permission, so that your login token can
   start a workflow.
4. Check the repository names in `/me/settings#workflows`. They default to
   `<login>/nixpkgs-review-gha` and `<login>/nixpkgs`.

### Move the pin of the reusable workflow

### Move the pin of the reusable workflow

`gha/update.yml` calls the reusable workflow at a full commit SHA, not at `main`. A change on `main` therefore does not run with your `GH_TOKEN` until you move the pin. To move the pin:

1. Find the newest commit that changes the reusable workflow: `gh api 'repos/numtide/maintainermode/commits?path=.github/workflows/update.yml&per_page=1' --jq '.[0].sha'`.
2. Read the change at that commit.
3. In `.github/workflows/update.yml` of your workflow repository, put the new SHA after `@` on the `uses:` line.
4. Change the comment after the SHA to `# main of <date>`, with the date of that commit on `main`.
5. Run `nix run nixpkgs#actionlint` on the file, then commit it.

## LLMs, if you want them

`/me/settings#llm` has one choice: **"I am OK with LLMs using my data on this
site"**. It is off by default. Each person sets it for themselves. When it is
off, nothing below is available to that person.

### The MCP server

`/mcp` is a read-only [MCP](https://modelcontextprotocol.io) server. It uses
JSON-RPC 2.0 over the Streamable HTTP transport of the MCP specification
2025-06-18.

- **The token.** Make it in `/me/settings#mcp`. The server shows it once and keeps
  only its SHA-256 hash. A new token replaces the old one. One button revokes
  it. Turning the choice off also revokes it.
- **A request** is a POST with one JSON-RPC message and
  `Authorization: Bearer <token>`. The answer is one JSON object. There is no
  session and no server-sent stream, so `GET /mcp` answers 405.
- **Methods:** `initialize`, `ping`, `tools/list` and `tools/call`.
- **Tools.** Each tool only reads. Each returns its JSON as text content. No
  tool writes to GitHub.

| Tool          | Returns                                                                  |
| ------------- | ------------------------------------------------------------------------ |
| `search`      | packages, modules and areas that match a name, with their health flags   |
| `package`     | one package, as `/api/v1/packages/<attr>`                                |
| `module`      | one NixOS module, as `/api/v1/modules/<file>`; `nixos/<name>` also works |
| `area`        | one path of the tree, as `/api/v1/areas/<path>`                          |
| `person`      | the record of a login and what it maintains, as `/api/v1/people/<login>` |
| `prs`         | one queue, as `/api/v1/prs`; `queue` is `foryou` by default              |
| `watching`    | your subscriptions, with their health flags                              |
| `sync_status` | the data of `/state?tab=status`                                          |

| Code | Meaning                                                       |
| ---- | ------------------------------------------------------------- |
| 200  | a JSON-RPC answer                                             |
| 202  | a notification; there is no answer                            |
| 400  | an `MCP-Protocol-Version` other than 2025-06-18 or 2025-03-26 |
| 401  | no token, or an unknown token                                 |
| 403  | the choice is off, or an `Origin` other than this site        |

To add it to Claude Code on your machine:

```sh
claude mcp add --transport http maintainer-mode https://<host>/mcp --header "Authorization: Bearer <token>"
```

### A Claude Code session on a pull request

Each open pull request row gets a **Claude Code** button next to **Review in
CI**. It opens `/claude/pr/<number>`. That page shows the prompt in a copy
box. Its link opens [Claude Code on the web](https://claude.ai/code) with the
prompt filled in and `NixOS/nixpkgs` selected, through the documented
`prompt` and `repositories` parameters. Nothing runs until you send the
prompt. The session runs in your own Claude account.

The prompt gives:

- the pull request number, its URL, its base branch and the files it changes.
  The title and the file paths come from the author. The prompt puts them in a
  fenced block and marks that block as data, not instructions;
- the nixpkgs
  [automation/AI policy](https://github.com/NixOS/nixpkgs/blob/master/CONTRIBUTING.md#automationai-policy):
  you review and answer for the result, each commit ends with an
  `Assisted-by:` trailer that names the tool and the model with its version,
  and review comments and descriptions disclose AI use apart from the commits;
- a rule: do not push to nixpkgs, and do nothing on GitHub without your word;
- the `/mcp` address, when you have an MCP token. The token itself never goes
  into the prompt.
