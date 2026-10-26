"use strict";

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const net = require("node:net");
const crypto = require("node:crypto");
const { spawn, spawnSync } = require("node:child_process");
const { chromium } = require("playwright-core");

const USAGE = `ux-audit: screenshot and check every page of Maintainer Mode

Usage: ux-audit [options]

Target (one of):
  --url <base>         a running server (default http://127.0.0.1:8790)
  --local              start maintainer-mode on a temporary database and port,
                       and audit it signed out and signed in

Options:
  --bin <path>         the maintainer-mode binary for --local
                       (default $MAINTAINER_MODE_BIN, else target/debug/maintainer-mode)
  --seed               with --local: fill the database with a small index and
                       the packages, machines, watches and follow-up plans
                       of the signed-in person
  --serve              with --local: no audit; keep the server running and
                       print its address and a session cookie, until Ctrl-C
  --out <dir>          output folder (default ./ux-audit)
  --pages <regex>      audit only the pages whose path matches
  --only-public        skip the signed-in pages
  --pages-file <file>  the page list (default nix/ux-pages.txt, else the packaged copy)
  --set NAME=VALUE     change a sample value of the page list (repeatable)
  --link-cap <n>       check at most n same-origin links (default 300)
  -h, --help           show this text

With --url, MAINTAINER_MODE_AUDIT_SESSION gives the value of the session
cookie for the signed-in pages. Without it, the signed-in pages are skipped.
`;

const VIEWPORTS = [
  { name: "390x844", width: 390, height: 844, mobile: true },
  { name: "1280x900", width: 1280, height: 900, mobile: false },
];
const SCHEMES = ["light", "dark"];
const LINK_SKIP = [
  "/auth/",
  "/feed/",
  "/pr/",
  "/claude/",
  "/api/v1/prs/",
  "/api/v1/events",
];
const SLOW_MS = 2000;
const HEAVY_BYTES = 512 * 1024;
const SEVERITY = { high: 0, medium: 1, low: 2 };

function parseArgs(argv) {
  const o = {
    url: null,
    local: false,
    seed: false,
    bin: process.env.MAINTAINER_MODE_BIN || null,
    out: "ux-audit",
    pages: null,
    onlyPublic: false,
    pagesFile: null,
    set: {},
    linkCap: 300,
  };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const val = () => {
      if (i + 1 >= argv.length) die(`${a} needs a value`);
      return argv[++i];
    };
    switch (a) {
      case "--url":
        o.url = val().replace(/\/+$/, "");
        break;
      case "--seed":
        o.seed = true;
        break;
      case "--serve":
        o.serve = true;
        break;
      case "--local":
        o.local = true;
        break;
      case "--bin":
        o.bin = val();
        break;
      case "--out":
        o.out = val();
        break;
      case "--pages":
        o.pages = new RegExp(val());
        break;
      case "--only-public":
        o.onlyPublic = true;
        break;
      case "--pages-file":
        o.pagesFile = val();
        break;
      case "--set": {
        const [k, ...v] = val().split("=");
        if (!v.length) die("--set takes NAME=VALUE");
        o.set[k] = v.join("=");
        break;
      }
      case "--link-cap":
        o.linkCap = Number.parseInt(val(), 10);
        if (!(o.linkCap >= 0)) die("--link-cap takes a number");
        break;
      case "-h":
      case "--help":
        process.stdout.write(USAGE);
        process.exit(0);
        break;
      default:
        die(`unknown option ${a}\n\n${USAGE}`);
    }
  }
  if (o.local && o.url) die("use --url or --local, not both");
  if (o.seed && !o.local) die("--seed needs --local");
  if (o.serve && !o.local) die("--serve needs --local");
  if (!o.local && !o.url) o.url = "http://127.0.0.1:8790";
  return o;
}

function die(msg) {
  process.stderr.write(`ux-audit: ${msg}\n`);
  process.exit(2);
}

function log(msg) {
  process.stderr.write(`${msg}\n`);
}

function readPages(file, overrides) {
  const vars = {};
  const pages = [];
  for (const raw of fs.readFileSync(file, "utf8").split("\n")) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;
    const w = line.split(/\s+/);
    if (w[0] === "set" && w.length === 3) {
      vars[w[1]] = w[2];
    } else if (w[0] === "skip") {
      continue;
    } else if (["public", "signed-in", "both"].includes(w[0])) {
      const opts = Object.fromEntries(w.slice(2).map((x) => x.split("=", 2)));
      pages.push({
        kind: w[0],
        template: w[1],
        status: opts.status ? Number(opts.status) : 200,
      });
    } else {
      die(`${file}: cannot read ${JSON.stringify(line)}`);
    }
  }
  Object.assign(vars, overrides);
  for (const p of pages) {
    p.path = p.template.replace(/\$\{(\w+)\}/g, (m, name) => {
      if (!(name in vars)) die(`${file}: unknown variable ${m}`);
      return vars[name];
    });
  }
  return pages;
}

function findPagesFile(o) {
  if (o.pagesFile) return o.pagesFile;
  if (fs.existsSync("nix/ux-pages.txt")) return "nix/ux-pages.txt";
  if (process.env.UX_AUDIT_PAGES) return process.env.UX_AUDIT_PAGES;
  die("no page list: pass --pages-file");
}

function freePort() {
  return new Promise((resolve, reject) => {
    const s = net.createServer();
    s.unref();
    s.on("error", reject);
    s.listen(0, "127.0.0.1", () => {
      const { port } = s.address();
      s.close(() => resolve(port));
    });
  });
}

async function waitFor(url, ms, child) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (child && child.exitCode !== null) return false;
    try {
      const r = await fetch(url);
      if (r.ok) return true;
    } catch {}
    await new Promise((r) => setTimeout(r, 250));
  }
  return false;
}

function sqlite(db, sql) {
  const r = spawnSync("sqlite3", [db, sql], { encoding: "utf8" });
  if (r.error) die(`sqlite3: ${r.error.message}`);
  if (r.status !== 0) die(`sqlite3: ${r.stderr.trim()}`);
  return r.stdout;
}

function cleanEnv() {
  const env = { ...process.env };
  for (const k of Object.keys(env))
    if (/^(GITHUB_|MAINTAINER_MODE_)/.test(k)) delete env[k];
  return env;
}

const SEED_LOGIN = "ux-audit";

// A small index, health and personal data, so the lists of a person who
// signed in are not empty. Only for the temporary database of --local.
function seedSql() {
  const pkg = (pname, version, maintainers) => ({
    pname,
    version,
    file: `pkgs/by-name/${pname.slice(0, 2)}/${pname}/package.nix`,
    maintainers,
    teams: [],
  });
  const pkgs = {
    hello: pkg("hello", "2.12.1", [SEED_LOGIN]),
    curl: {
      ...pkg("curl", "8.9.1", ["someone"]),
      facts: {
        description: "Command line tool for transferring files with URL syntax",
        homepage: true,
        license: true,
        main_program: true,
        src_url:
          "https://github.com/curl/curl/releases/download/curl-8_9_1/curl-8.9.1.tar.xz",
      },
      meta: { changelog: ["https://curl.se/ch/8.9.1.html"] },
    },
    openssl: pkg("openssl", "3.0.14", []),
    jq: pkg("jq", "1.7.1", [SEED_LOGIN]),
    unbound: pkg("unbound", "1.26.1", ["someone"]),
    ripgrep: pkg("ripgrep", "14.1.0", ["someone"]),
    tinyfetch: pkg("tinyfetch", "0.3.0", []),
    "a-package-with-a-very-long-name-for-the-phone-layout": pkg(
      "a-package-with-a-very-long-name-for-the-phone-layout",
      "0.1.0-unstable-2026-01-01",
      [SEED_LOGIN],
    ),
  };
  const index = {
    rev: "2222222222222222222222222222222222222222",
    maintainers: {},
    owners: [["pkgs/by-name", [SEED_LOGIN]]],
    by_pname: Object.fromEntries(Object.keys(pkgs).map((a) => [a, a])),
    pkgs,
    modules: {
      "nixos/modules/services/web-servers/nginx/default.nix": {
        maintainers: [SEED_LOGIN],
        teams: [],
      },
    },
    teams: {},
  };
  const health = {
    upstream: { curl: "8.10.0", jq: "1.8.0" },
    advisories: {
      openssl: [
        {
          code: "NIXPKGS-2026-0001",
          cves: ["CVE-2026-0001"],
          title: "openssl: a buffer overflow in the TLS handshake",
          issue: null,
        },
      ],
      curl: [
        {
          code: "NIXPKGS-2026-0002",
          cves: ["CVE-2024-8096"],
          title: "curl: OCSP stapling bypass with GnuTLS",
          issue: null,
        },
      ],
      unbound: [
        {
          code: "NIXPKGS-2026-2615",
          cves: ["CVE-2026-0003"],
          title: "unbound: security issues < 1.26.1",
          issue: 564154,
          ranges: [
            {
              cve: "CVE-2026-0003",
              constraints: [["affected", "<1.26.1"]],
            },
          ],
        },
      ],
    },
    cves: {
      "CVE-2026-0001": { band: "critical", cvss: 9.8, epss: 0.35 },
      "CVE-2026-0003": { band: "critical", cvss: 9.8 },
    },
  };
  const set = ["curl", "openssl", "hello", "ripgrep", "unbound"];
  // a long label and more hosts than a row shows
  const infraOutputs = {
    labels: [
      "web-1",
      "web-2",
      "web-3",
      "build-box-in-the-basement-with-a-long-name",
      "me@laptop",
      "devShells.default",
    ],
    of: {
      curl: [0, 1, 2, 3, 4],
      openssl: [0, 1, 2],
      unbound: [0, 3],
      hello: [5],
      ripgrep: [4, 5],
    },
  };
  const q = (v) => `'${JSON.stringify(v).replace(/'/g, "''")}'`;
  const now = new Date().toISOString();
  const daysAgo = (d) => new Date(Date.now() - d * 86400000).toISOString();
  const file = (p) => pkgs[p].file;
  const pr = (number, author, title, opened, extra) => ({
    number,
    title,
    url: `https://github.com/NixOS/nixpkgs/pull/${number}`,
    author,
    author_is_bot: false,
    state: "OPEN",
    is_draft: false,
    base: "master",
    created_at: daysAgo(opened),
    updated_at: daysAgo(0),
    merged_at: null,
    merged_by: null,
    mergeable: "MERGEABLE",
    head_oid: `${number}`.padStart(40, "0"),
    files: [],
    ci: "SUCCESS",
    reviews: [],
    author_activity: [],
    pushes: [daysAgo(opened)],
    voids: [],
    ...extra,
  });
  const review = (number, author, state, at, inline = false) => ({
    url: `https://github.com/NixOS/nixpkgs/pull/${number}#pullrequestreview-${number}${at.length}`,
    author,
    author_is_bot: false,
    state,
    submitted_at: at,
    commit_oid: `${number}`.padStart(40, "0"),
    inline_on_change: inline,
  });
  // a reviewer with credit from three authors, so their own pull request
  // is in the review queue; the first merge makes the seed login a
  // committer
  const merged = ["one", "two", "three"].map((a, i) =>
    pr(100160 + i, `author-${a}`, `ripgrep: small fix ${a}`, 20 + i, {
      state: "MERGED",
      merged_at: daysAgo(10 + i),
      merged_by: i === 0 ? SEED_LOGIN : "someone",
      files: [file("ripgrep")],
      reviews: [
        review(100160 + i, "contributor", "COMMENTED", daysAgo(12 + i), true),
      ],
    }),
  );
  const prs = [
    ...merged,
    pr(100171, SEED_LOGIN, "hello: 2.12.1 -> 2.12.2", 6, {
      pushes: [daysAgo(4)],
      files: [file("hello")],
      reviews: [review(100171, "someone", "CHANGES_REQUESTED", daysAgo(1))],
    }),
    pr(100172, SEED_LOGIN, "jq: 1.7.1 -> 1.8.0", 2, {
      ci: "FAILURE",
      files: [file("jq")],
    }),
    pr(100173, SEED_LOGIN, "ripgrep: enable the pcre2 feature", 9, {
      files: [file("ripgrep")],
    }),
    pr(100175, "r-ryantm", "tinyfetch: 0.3.0 -> 0.3.1", 4, {
      author_is_bot: true,
      files: [file("tinyfetch")],
    }),
    pr(100174, "contributor", "curl: 8.9.1 -> 8.10.0 (CVE-2024-8096)", 3, {
      files: [file("curl")],
      mergeable: "CONFLICTING",
      ci: "FAILURE",
      additions: 2,
      deletions: 2,
      changed_files: 1,
      labels: [
        "10.rebuild-linux: 1001-2500",
        "10.rebuild-darwin: 501-1000",
        "1.severity: security",
      ],
      association: "CONTRIBUTOR",
    }),
    pr(100176, "newcomer", "ripgrep: add shell completions", 300, {
      files: [file("ripgrep")],
      mergeable: "CONFLICTING",
      ci: "FAILURE",
      additions: 12,
      deletions: 1,
      changed_files: 1,
      labels: ["12.first-time contribution"],
      association: "FIRST_TIME_CONTRIBUTOR",
    }),
  ];
  return [
    `INSERT OR REPLACE INTO meta (key, value) VALUES ('index', ${q(index)});`,
    `INSERT OR REPLACE INTO meta (key, value) VALUES ('health', ${q(health)});`,
    `INSERT OR REPLACE INTO package_set (login, name, attrs, dropped, uploaded_at, scanned_at, nixpkgs_rev) VALUES ('${SEED_LOGIN}', 'laptop', ${q(set)}, 3, '${now}', '${now}', '${index.rev}');`,
    `INSERT OR REPLACE INTO package_set (login, name, attrs, dropped, uploaded_at, scanned_at, nixpkgs_rev) VALUES ('${SEED_LOGIN}', 'web-1', ${q(set.slice(0, 2))}, 0, '${daysAgo(3)}', '${daysAgo(3)}', '${index.rev}');`,
    `INSERT OR REPLACE INTO package_set (login, name, attrs, dropped, uploaded_at, scanned_at, nixpkgs_rev, outputs) VALUES ('${SEED_LOGIN}', 'infra', ${q(set)}, 2, '${daysAgo(2)}', '${daysAgo(2)}', '${index.rev}', ${q(infraOutputs)});`,
    `INSERT OR REPLACE INTO package_set (login, name, attrs, dropped, uploaded_at, scanned_at, nixpkgs_rev, reference, output, public) VALUES ('${SEED_LOGIN}', 'website', ${q(set.slice(1))}, 1, '${daysAgo(1)}', '${daysAgo(1)}', '${index.rev}', 'github:numtide/maintainermode/main', 'nixosConfigurations.a-rather-long-host-name.config.system.build.toplevel', 1);`,
    ...prs.map(
      (p) =>
        `INSERT OR REPLACE INTO pr (number, updated_at, state, data) VALUES (${p.number}, '${p.updated_at}', '${p.state}', ${q(p)});`,
    ),
    ...[
      "pkg:ripgrep",
      "pkg:openssl",
      "area:pkgs/development/python-modules",
    ].map(
      (k) =>
        `INSERT OR IGNORE INTO watch (login, package, created_at) VALUES ('${SEED_LOGIN}', '${k}', '${now}');`,
    ),
    ...[
      [100173, 7, "merge", daysAgo(8)],
      [100172, 3, "merge", now],
    ].map(
      ([n, days, action, since]) =>
        `INSERT OR REPLACE INTO followup (login, number, days, action, since) VALUES ('${SEED_LOGIN}', ${n}, ${days}, '${action}', '${since}');`,
    ),
  ].join("\n");
}

async function startLocal(o) {
  let bin = o.bin;
  if (!bin && fs.existsSync("target/debug/maintainer-mode"))
    bin = "target/debug/maintainer-mode";
  if (!bin)
    die(
      "--local needs the binary: pass --bin, set MAINTAINER_MODE_BIN, or run cargo build",
    );
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ux-audit-"));
  const db = path.join(dir, "maintainer-mode.sqlite");
  const port = await freePort();
  const base = `http://127.0.0.1:${port}`;
  const logFile = path.join(dir, "server.log");
  const out = fs.openSync(logFile, "w");
  const blackhole = "http://127.0.0.1:9";
  const spawnServer = () =>
    spawn(
      bin,
      [
        "serve",
        "--db",
        db,
        "--listen",
        `127.0.0.1:${port}`,
        "--public-url",
        base,
      ],
      {
        stdio: ["ignore", out, out],
        env: {
          ...cleanEnv(),
          HTTP_PROXY: blackhole,
          HTTPS_PROXY: blackhole,
          ALL_PROXY: blackhole,
          http_proxy: blackhole,
          https_proxy: blackhole,
          all_proxy: blackhole,
          NO_PROXY: "127.0.0.1,localhost",
          no_proxy: "127.0.0.1,localhost",
        },
      },
    );
  let child = spawnServer();
  const stop = () => {
    if (child.exitCode === null) child.kill("SIGTERM");
    fs.rmSync(dir, { recursive: true, force: true });
  };
  const started = async () => {
    if (!(await waitFor(`${base}/healthz`, 60000, child))) {
      const tail = fs.readFileSync(logFile, "utf8").split("\n").slice(-20);
      stop();
      die(`the server did not start:\n${tail.join("\n")}`);
    }
  };
  await started();
  if (o.seed) {
    const exited = new Promise((r) => child.once("exit", r));
    child.kill("SIGTERM");
    await exited;
    sqlite(db, seedSql());
    child = spawnServer();
    await started();
    log(`seeded the local database for ${SEED_LOGIN}`);
  }
  const session = crypto.randomBytes(32).toString("hex");
  const login = SEED_LOGIN;
  const ensureSession = () => {
    const now = new Date().toISOString();
    sqlite(
      db,
      `INSERT OR IGNORE INTO session (id, login, token, created_at, seen_at) VALUES ('${session}', '${login}', 'ux-audit-dummy-token', '${now}', '${now}');`,
    );
  };
  ensureSession();
  log(`local server on ${base}, database ${db}`);
  return { base, session, ensureSession, stop };
}

function audit(deviceWidth) {
  const W = deviceWidth;
  const sel = (el) => {
    let s = el.tagName.toLowerCase();
    if (el.id) s += `#${el.id}`;
    const cls = [...el.classList].slice(0, 2);
    if (cls.length) s += `.${cls.join(".")}`;
    return s;
  };
  const text = (el) => (el.textContent || "").replace(/\s+/g, " ").trim();
  const short = (s) => (s.length > 60 ? `${s.slice(0, 57)}...` : s);
  const name = (el) =>
    (el.getAttribute("aria-label") || "").trim() ||
    (el.getAttribute("aria-labelledby") || "")
      .split(/\s+/)
      .map((id) => document.getElementById(id))
      .filter(Boolean)
      .map(text)
      .join(" ")
      .trim() ||
    (el.getAttribute("title") || "").trim() ||
    text(el) ||
    [...el.querySelectorAll("img[alt]")]
      .map((i) => i.alt)
      .join(" ")
      .trim();

  const scrollWidth = document.documentElement.scrollWidth;
  const overflow = scrollWidth > W;
  const wide = [];
  if (overflow) {
    for (const el of document.body.querySelectorAll("*")) {
      const r = el.getBoundingClientRect();
      if (r.width === 0 || r.right <= W + 1) continue;
      const p = el.parentElement;
      if (p && p !== document.body && p.getBoundingClientRect().right > W + 1)
        continue;
      wide.push(`${sel(el)} (${Math.round(r.right)}px)`);
      if (wide.length >= 3) break;
    }
  }

  const imgNoAlt = [
    ...document.querySelectorAll(
      "img:not([alt]), input[type=image]:not([alt])",
    ),
  ].map((el) => short(el.getAttribute("src") || sel(el)));

  const skipTypes = ["hidden", "submit", "button", "reset", "image"];
  const noLabel = [...document.querySelectorAll("input, select, textarea")]
    .filter((el) => !skipTypes.includes((el.type || "").toLowerCase()))
    .filter(
      (el) =>
        !(el.labels && el.labels.length) &&
        !(el.getAttribute("aria-label") || "").trim() &&
        !el.getAttribute("aria-labelledby") &&
        !(el.getAttribute("title") || "").trim(),
    )
    .map(
      (el) =>
        `${sel(el)}${el.name ? `[name=${el.name}]` : ""}${el.type ? `[type=${el.type}]` : ""}`,
    );

  const noName = [
    ...document.querySelectorAll(
      "button, a[href], [role=button], input[type=submit], input[type=button]",
    ),
  ]
    .filter((el) => !(el.value && el.tagName === "INPUT") && !name(el))
    .map((el) =>
      el.tagName === "A" ? `a[href=${el.getAttribute("href")}]` : sel(el),
    );

  const heads = [...document.querySelectorAll("h1, h2, h3, h4, h5, h6")].map(
    (h) => ({ level: Number(h.tagName[1]), text: short(text(h)) }),
  );
  const h1 = heads.filter((h) => h.level === 1).length;
  const skips = [];
  let prev = 0;
  for (const h of heads) {
    if (prev && h.level > prev + 1)
      skips.push(`h${prev} then h${h.level} "${h.text}"`);
    prev = h.level;
  }

  const cv = document.createElement("canvas");
  cv.width = cv.height = 1;
  const cx = cv.getContext("2d", { willReadFrequently: true });
  const rgba = (c) => {
    cx.clearRect(0, 0, 1, 1);
    cx.fillStyle = "#000";
    cx.fillStyle = c;
    cx.fillRect(0, 0, 1, 1);
    const d = cx.getImageData(0, 0, 1, 1).data;
    return [d[0], d[1], d[2], d[3] / 255];
  };
  const over = (top, bottom) => [
    top[0] * top[3] + bottom[0] * (1 - top[3]),
    top[1] * top[3] + bottom[1] * (1 - top[3]),
    top[2] * top[3] + bottom[2] * (1 - top[3]),
    1,
  ];
  const lum = (c) => {
    const [r, g, b] = c.slice(0, 3).map((v) => {
      v /= 255;
      return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
    });
    return 0.2126 * r + 0.7152 * g + 0.0722 * b;
  };
  const hex = (c) =>
    `#${c
      .slice(0, 3)
      .map((v) => Math.round(v).toString(16).padStart(2, "0"))
      .join("")}`;
  const background = (el) => {
    const layers = [];
    for (let e = el; e; e = e.parentElement) {
      const cs = getComputedStyle(e);
      if (cs.backgroundImage !== "none") return null;
      const c = rgba(cs.backgroundColor);
      if (c[3] > 0) layers.push(c);
      if (c[3] >= 1) break;
    }
    let bg = [255, 255, 255, 1];
    for (const l of layers.reverse()) bg = over(l, bg);
    return bg;
  };
  const pairs = new Map();
  for (const el of document.body.querySelectorAll("*")) {
    if (el.closest("[aria-hidden=true], svg, [disabled]")) continue;
    const own = [...el.childNodes].some(
      (n) => n.nodeType === 3 && n.textContent.trim(),
    );
    if (!own) continue;
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) continue;
    const cs = getComputedStyle(el);
    if (cs.visibility === "hidden" || Number(cs.opacity) === 0) continue;
    const bg = background(el);
    if (!bg) continue;
    const fg = over(rgba(cs.color), bg);
    const [a, b] = [lum(fg), lum(bg)].sort((x, y) => y - x);
    const ratio = (a + 0.05) / (b + 0.05);
    const size = Number.parseFloat(cs.fontSize);
    const large = size >= 24 || (size >= 18.66 && Number(cs.fontWeight) >= 700);
    if (ratio >= (large ? 3 : 4.5)) continue;
    const key = `${hex(fg)} on ${hex(bg)}`;
    if (!pairs.has(key))
      pairs.set(
        key,
        `${ratio.toFixed(2)}:1 ${key}, e.g. ${sel(el)} "${short(text(el))}"`,
      );
  }
  const lowContrast = [...pairs.values()].slice(0, 5);

  const smallTargets = [];
  if (W < 600) {
    for (const el of document.querySelectorAll(
      "button, a[href], input:not([type=hidden]), select, summary, [role=button]",
    )) {
      const cs = getComputedStyle(el);
      if (el.tagName === "A" && cs.display === "inline") continue;
      // a click on the label of a checkbox or radio works it: the label is the target
      const label =
        el.tagName === "INPUT" &&
        (el.type === "checkbox" || el.type === "radio") &&
        el.closest("label");
      const r = (label || el).getBoundingClientRect();
      if (r.width === 0 || r.height === 0) continue;
      if (r.width < 24 || r.height < 24)
        smallTargets.push(
          `${el.tagName === "A" ? `a[href=${el.getAttribute("href")}]` : sel(el)} ${Math.round(r.width)}x${Math.round(r.height)}`,
        );
    }
  }

  const ids = {};
  for (const el of document.querySelectorAll("[id]"))
    ids[el.id] = (ids[el.id] || 0) + 1;
  const dupIds = Object.entries(ids)
    .filter(([, n]) => n > 1)
    .map(([id, n]) => `${id} (${n}x)`);

  const links = [
    ...new Set(
      [...document.querySelectorAll("a[href]")]
        .map((a) => {
          try {
            const u = new URL(a.getAttribute("href"), location.href);
            if (u.origin !== location.origin) return null;
            u.hash = "";
            return u.pathname + u.search;
          } catch {
            return null;
          }
        })
        .filter(Boolean),
    ),
  ];

  return {
    title: document.title,
    scrollWidth,
    innerWidth: window.innerWidth,
    viewportMeta: !!document.querySelector("meta[name=viewport]"),
    deviceWidth,
    overflow,
    wide,
    imgNoAlt,
    noLabel,
    noName,
    h1,
    headings: heads.map((h) => `h${h.level}`).join(" "),
    skips,
    dupIds,
    lowContrast,
    smallTargets,
    links,
  };
}

function slug(s) {
  const h = crypto.createHash("sha1").update(s).digest("hex").slice(0, 6);
  const base =
    s
      .replace(/^\/+/, "")
      .replace(/[^A-Za-z0-9]+/g, "-")
      .replace(/^-|-$/g, "")
      .slice(0, 60) || "home";
  return `${base}-${h}`;
}

// The shortcuts of src/keys.js, pressed on a desktop page. A failed step
// is a high issue: a keyboard user is stuck.
async function checkKeys(ctx, base, state, issues, outDir) {
  const shot = (name) =>
    page.screenshot({
      path: path.join(outDir, "shots", `keys-${name}--${state}.png`),
    });
  const page = await ctx.newPage();
  let failed = 0;
  const fail = (step, detail) => {
    failed++;
    issues.add("high", "keyboard", step, state, detail, "1280x900 light");
  };
  const at = () => new URL(page.url()).pathname;
  try {
    await page.goto(`${base}/`, { waitUntil: "load" });
    await page.keyboard.press("/");
    if (
      !(await page.evaluate(() =>
        document.activeElement?.matches("[data-search]"),
      ))
    )
      fail("/", "/ does not focus the search box");
    await page.keyboard.type("hel");
    try {
      await page.waitForSelector(".suggest:not([hidden]) li", {
        timeout: 3000,
      });
      await page.keyboard.press("ArrowDown");
      await shot("suggest");
      await page.keyboard.press("Enter");
      await page.waitForURL((u) => u.pathname !== "/", { timeout: 3000 });
      if (!/^\/(p|m|a|u|pr|search)\b/.test(at()))
        fail("/", `the first suggestion opens ${at()}`);
    } catch (e) {
      fail("/", `no suggestion to open: ${e.message.split("\n")[0]}`);
    }
    try {
      await page.goto(`${base}/`, { waitUntil: "load" });
      await page.keyboard.press("/");
      await page.keyboard.type("hel");
      const li = await page.waitForSelector(".suggest:not([hidden]) li a", {
        timeout: 3000,
      });
      const box = await li.boundingBox();
      await page.mouse.move(box.x + 8, box.y + box.height / 2);
      await page.mouse.down();
      await page.waitForTimeout(400);
      await page.mouse.up();
      await page.waitForURL((u) => u.pathname !== "/", { timeout: 3000 });
    } catch (e) {
      fail(
        "/",
        `a slow click on a suggestion opens nothing: ${e.message.split("\n")[0]}`,
      );
    }
    for (const [keys, want] of [
      [["g", "r"], "/review"],
      [["g", "p"], "/packages"],
      [["g", "s"], "/state"],
      [["g", "h"], "/"],
    ]) {
      for (const k of keys) await page.keyboard.press(k);
      await page
        .waitForURL((u) => u.pathname === want, { timeout: 3000 })
        .catch(() => {});
      if (at() !== want)
        fail(want, `${keys.join(" ")} opens ${at()}, not ${want}`);
    }
    await page.goto(`${base}/review?tab=all`, { waitUntil: "load" });
    await page.keyboard.press("j");
    const href = await page.evaluate(() =>
      document.querySelector(".key-sel a.Link--primary")?.getAttribute("href"),
    );
    if (!href) fail("/review?tab=all", "j selects no row");
    else {
      await page.keyboard.press("o");
      await page
        .waitForURL((u) => u.pathname === new URL(href, base).pathname, {
          timeout: 3000,
        })
        .catch(() => {});
      if (at() !== new URL(href, base).pathname)
        fail("/review?tab=all", `o opens ${at()}, not ${href}`);
    }
    await page.keyboard.press("Shift+?");
    if (
      !(await page.evaluate(
        () => !!document.querySelector("dialog.keys-help[open]"),
      ))
    )
      fail(at(), "? shows no list of shortcuts");
    else await shot("help");
    await page.keyboard.press("Escape");
    await page.keyboard.press("Tab");
    if (
      !(await page.evaluate(() =>
        document.activeElement?.matches(".skip-link"),
      ))
    )
      fail(at(), "the first Tab does not reach Skip to content");
  } catch (e) {
    fail("keys", e.message.split("\n")[0]);
  } finally {
    await page.close();
    log(`keyboard ${state}: ${failed} of 10 steps fail`);
  }
}
class Issues {
  constructor() {
    this.map = new Map();
  }
  add(severity, kind, page, state, detail, variant) {
    const key = [severity, kind, page, state, detail].join("\u0000");
    let i = this.map.get(key);
    if (!i) {
      i = { severity, kind, page, state, detail, variants: [] };
      this.map.set(key, i);
    }
    if (variant && !i.variants.includes(variant)) i.variants.push(variant);
  }
  sorted() {
    return [...this.map.values()].sort(
      (a, b) =>
        SEVERITY[a.severity] - SEVERITY[b.severity] ||
        a.kind.localeCompare(b.kind) ||
        a.page.localeCompare(b.page),
    );
  }
}

async function shoot(ctx, base, page, state, vp, scheme, outDir, issues) {
  const url = base + page.path;
  const variant = `${vp.name} ${scheme}`;
  const tab = await ctx.newPage();
  const consoleErrors = [];
  const failed = [];
  tab.on("console", (m) => {
    if (m.type() === "error" && !m.text().startsWith("Failed to load resource"))
      consoleErrors.push(m.text().slice(0, 300));
  });
  tab.on("pageerror", (e) => consoleErrors.push(String(e).slice(0, 300)));
  tab.on("requestfailed", (r) => {
    const why = r.failure() ? r.failure().errorText : "failed";
    if (why.startsWith("net::ERR_BLOCKED_BY_CLIENT") || why.includes("aborted"))
      return;
    failed.push(`${r.url()} (${why})`);
  });
  tab.on("response", (r) => {
    if (
      r.request().isNavigationRequest() &&
      r.request().frame() === tab.mainFrame()
    )
      return;
    if (r.status() >= 400) failed.push(`${r.url()} (${r.status()})`);
  });
  const shot = {
    viewport: vp.name,
    scheme,
    status: null,
    finalUrl: null,
    bytes: null,
    ms: null,
    png: null,
  };
  try {
    const t0 = Date.now();
    const resp = await tab.goto(url, { waitUntil: "load", timeout: 45000 });
    shot.ms = Date.now() - t0;
    shot.status = resp ? resp.status() : null;
    shot.finalUrl = tab.url();
    try {
      shot.bytes = resp ? (await resp.body()).length : null;
    } catch {}
    const a = await tab.evaluate(audit, vp.width);
    Object.assign(shot, a);
    const file = `${slug(page.path)}--${state}--${vp.name}--${scheme}.png`;
    try {
      await tab.screenshot({
        path: path.join(outDir, "shots", file),
        fullPage: true,
        timeout: 60000,
      });
    } catch (e) {
      await tab.screenshot({ path: path.join(outDir, "shots", file) });
      issues.add(
        "low",
        "screenshot cut to the viewport",
        page.path,
        state,
        String(e).split("\n")[0],
        variant,
      );
    }
    shot.png = `shots/${file}`;
  } catch (e) {
    shot.error = String(e).split("\n")[0];
    issues.add(
      "high",
      "page does not load",
      page.path,
      state,
      shot.error,
      variant,
    );
  }
  shot.consoleErrors = consoleErrors;
  shot.failedRequests = failed;
  await tab.close();

  const p = page.path;
  if (shot.status !== null && shot.status !== page.status)
    issues.add(
      "high",
      "unexpected HTTP status",
      p,
      state,
      `${shot.status}, expected ${page.status}`,
      variant,
    );
  if (
    shot.finalUrl &&
    new URL(shot.finalUrl).pathname + new URL(shot.finalUrl).search !== p
  )
    issues.add(
      "low",
      "redirects",
      p,
      state,
      `to ${shot.finalUrl.replace(base, "")}`,
      variant,
    );
  for (const c of consoleErrors)
    issues.add("high", "console error", p, state, c, variant);
  for (const f of failed)
    issues.add(
      "high",
      "failed request",
      p,
      state,
      f.replace(base, ""),
      variant,
    );
  if (shot.error) return shot;
  if (!shot.viewportMeta)
    issues.add(
      "medium",
      "no viewport meta tag",
      p,
      state,
      "phones lay the page out at 980px",
      null,
    );
  if (shot.overflow)
    issues.add(
      "medium",
      "horizontal overflow",
      p,
      state,
      `scrollWidth ${shot.scrollWidth} > ${shot.deviceWidth}${shot.wide.length ? `: ${shot.wide.join(", ")}` : ""}`
        .replace(/\(\d+px\)/g, "")
        .trim(),
      variant,
    );
  for (const x of shot.imgNoAlt)
    issues.add("medium", "image without alt", p, state, x, variant);
  for (const x of shot.noLabel)
    issues.add("medium", "form control without label", p, state, x, variant);
  for (const x of shot.noName)
    issues.add(
      "medium",
      "control without accessible name",
      p,
      state,
      x,
      variant,
    );
  for (const x of shot.dupIds)
    issues.add("medium", "duplicate id", p, state, x, variant);
  for (const x of shot.lowContrast)
    issues.add("medium", "low text contrast", p, state, x, variant);
  if (shot.smallTargets.length)
    issues.add(
      "low",
      "tap target under 24px",
      p,
      state,
      `${shot.smallTargets.length}: ${shot.smallTargets.slice(0, 3).join("; ")}`,
      variant,
    );
  if (shot.h1 !== 1 && shot.status === page.status)
    issues.add(
      "medium",
      "h1 count is not 1",
      p,
      state,
      `${shot.h1} h1 (${shot.headings || "no headings"})`,
      variant,
    );
  for (const x of shot.skips)
    issues.add("low", "skipped heading level", p, state, x, variant);
  if (shot.ms > SLOW_MS)
    issues.add("low", "slow load", p, state, `over ${SLOW_MS} ms`, variant);
  if (shot.bytes > HEAVY_BYTES)
    issues.add(
      "low",
      "heavy HTML",
      p,
      state,
      `${Math.round(shot.bytes / 1024)} KiB`,
      variant,
    );
  return shot;
}

async function checkLinks(found, ctxFor, base, known, cap, issues) {
  const todo = [...found.entries()].filter(
    ([u]) => !LINK_SKIP.some((s) => u.startsWith(s)) && !known.has(u),
  );
  const results = [];
  const queue = todo.slice(0, cap);
  if (todo.length > cap)
    log(`checking ${cap} of ${todo.length} links (--link-cap)`);
  const one = async ([u, info]) => {
    const req = ctxFor(info.state).request;
    let target = u;
    let status = null;
    let error = null;
    for (let hop = 0; hop < 5; hop++) {
      try {
        let r = await req.head(base + target, {
          maxRedirects: 0,
          timeout: 30000,
        });
        if (r.status() === 405 || r.status() === 501)
          r = await req.get(base + target, { maxRedirects: 0, timeout: 30000 });
        status = r.status();
        const loc = r.headers().location;
        if (status >= 300 && status < 400 && loc) {
          const next = new URL(loc, base + target);
          if (next.origin !== new URL(base).origin) break;
          target = next.pathname + next.search;
          if (LINK_SKIP.some((s) => target.startsWith(s))) break;
          continue;
        }
      } catch (e) {
        error = String(e).split("\n")[0];
      }
      break;
    }
    results.push({
      url: u,
      status,
      final: target,
      error,
      from: info.from.slice(0, 5),
      state: info.state,
    });
    if (error || status === null || status === 404 || status >= 500) {
      const what = error
        ? error
        : `${status}${target !== u ? ` via ${target}` : ""}`;
      issues.add(
        "high",
        "broken internal link",
        info.from[0],
        info.state,
        `${u} -> ${what}`,
        null,
      );
    }
  };
  const width = new URL(base).hostname === "127.0.0.1" ? 6 : 2;
  for (let i = 0; i < queue.length; i += width)
    await Promise.all(queue.slice(i, i + width).map(one));
  return { checked: results, unchecked: Math.max(0, todo.length - cap) };
}

const esc = (s) =>
  String(s).replace(
    /[&<>"']/g,
    (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[
        c
      ],
  );

function renderIndex(report) {
  const counts = { high: 0, medium: 0, low: 0 };
  for (const i of report.issues) counts[i.severity]++;
  const rows = report.issues
    .map(
      (i) =>
        `<tr class="${i.severity}"><td><span class="sev">${i.severity}</span></td><td>${esc(i.kind)}</td><td><a href="#${esc(slug(i.page))}--${i.state}">${esc(i.page)}</a><br><small>${i.state}</small></td><td>${esc(i.detail)}</td><td><small>${esc(i.variants.join(", ") || "-")}</small></td></tr>`,
    )
    .join("\n");
  const cards = report.pages
    .map((p) => {
      const first = p.shots.find((s) => s.status !== null) || {};
      const n = report.issues.filter(
        (i) => i.page === p.path && i.state === p.state,
      ).length;
      const shots = p.shots
        .map(
          (s) =>
            `<figure><figcaption>${s.viewport} ${s.scheme}${s.status !== null ? ` · ${s.status} · ${s.ms} ms` : ""}</figcaption>${
              s.png
                ? `<a href="${esc(s.png)}"><div class="frame${s.viewport.startsWith("390") ? " narrow" : ""}"><img loading="lazy" src="${esc(s.png)}" alt="${esc(`${p.path} ${p.state} ${s.viewport} ${s.scheme}`)}"></div></a>`
                : `<div class="frame missing">${esc(s.error || "no screenshot")}</div>`
            }</figure>`,
        )
        .join("");
      return `<section class="page" id="${esc(slug(p.path))}--${p.state}"><h3><a href="${esc(report.base + p.path)}">${esc(p.path)}</a> <small>${p.state} · ${esc(first.title || "")} · ${first.bytes != null ? `${Math.round(first.bytes / 1024)} KiB` : "-"} · ${n} issue${n === 1 ? "" : "s"}</small></h3><div class="shots">${shots}</div></section>`;
    })
    .join("\n");
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>UX audit</title>
<style>
:root { --bg: #fff; --fg: #1f2328; --muted: #59636e; --line: #d1d9e0; --card: #f6f8fa; --high: #cf222e; --medium: #9a6700; --low: #59636e; --link: #0969da; }
@media (prefers-color-scheme: dark) { :root { --bg: #0d1117; --fg: #e6edf3; --muted: #9198a1; --line: #3d444d; --card: #151b23; --high: #ff7b72; --medium: #d29922; --low: #9198a1; --link: #4493f8; } }
* { box-sizing: border-box; }
body { margin: 0; padding: 16px; background: var(--bg); color: var(--fg); font: 14px/1.45 system-ui, sans-serif; }
a { color: var(--link); }
h1 { margin: 0 0 4px; font-size: 22px; }
h2 { margin: 32px 0 8px; font-size: 18px; }
h3 { margin: 0 0 8px; font-size: 15px; word-break: break-all; }
small, .muted { color: var(--muted); font-weight: normal; }
.summary span { margin-right: 16px; }
.table { overflow-x: auto; }
table { border-collapse: collapse; width: 100%; }
th, td { text-align: left; vertical-align: top; padding: 6px 8px; border-bottom: 1px solid var(--line); word-break: break-word; }
.sev { font-weight: 600; text-transform: uppercase; font-size: 11px; }
tr.high .sev { color: var(--high); } tr.medium .sev { color: var(--medium); } tr.low .sev { color: var(--low); }
.page { margin: 16px 0; padding: 12px; background: var(--card); border: 1px solid var(--line); border-radius: 6px; }
.shots { display: grid; grid-template-columns: repeat(auto-fill, minmax(220px, 1fr)); gap: 12px; }
figure { margin: 0; }
figcaption { font-size: 12px; color: var(--muted); margin-bottom: 4px; }
.frame { height: 420px; overflow: hidden; border: 1px solid var(--line); border-radius: 4px; background: var(--bg); }
.frame img { display: block; width: 100%; height: auto; }
.frame.missing { display: flex; align-items: center; justify-content: center; padding: 8px; color: var(--high); }
</style>
</head>
<body>
<h1>UX audit</h1>
<p class="muted">${esc(report.base)} · ${esc(report.mode)} · ${esc(report.started)} · ${report.pages.length} page views · ${report.links.checked.length} links checked${report.links.unchecked ? `, ${report.links.unchecked} not checked` : ""}</p>
<p class="summary"><span><b>${counts.high}</b> high</span><span><b>${counts.medium}</b> medium</span><span><b>${counts.low}</b> low</span>${report.skipped.length ? `<span>${report.skipped.length} signed-in pages skipped: no session</span>` : ""}</p>
<h2>Issues</h2>
<div class="table"><table>
<thead><tr><th>Severity</th><th>Kind</th><th>Page</th><th>Detail</th><th>Where</th></tr></thead>
<tbody>
${rows || '<tr><td colspan="5">No issues.</td></tr>'}
</tbody>
</table></div>
<h2>Pages</h2>
${cards}
</body>
</html>
`;
}

async function main() {
  const o = parseArgs(process.argv.slice(2));
  const pagesFile = findPagesFile(o);
  let pages = readPages(pagesFile, o.set);
  if (o.pages) pages = pages.filter((p) => o.pages.test(p.path));
  if (!pages.length) die("no page matches");

  let base = o.url;
  let session = process.env.MAINTAINER_MODE_AUDIT_SESSION || null;
  let local = null;
  if (o.local) {
    local = await startLocal(o);
    base = local.base;
    session = local.session;
  }
  const cleanup = () => local && local.stop();
  process.on("SIGINT", () => {
    cleanup();
    process.exit(130);
  });
  if (o.serve) {
    log(`serving on ${base}`);
    log(`sign in: open ${base} and set the cookie session=${session}`);
    log(
      `  document.cookie = "session=${session}" in the console of the browser`,
    );
    log("Ctrl-C stops the server and removes its database");
    await new Promise(() => {});
    return;
  }

  const outDir = path.resolve(o.out);
  fs.rmSync(path.join(outDir, "shots"), { recursive: true, force: true });
  fs.mkdirSync(path.join(outDir, "shots"), { recursive: true });

  const jobs = [];
  const skipped = [];
  for (const p of pages) {
    if (p.kind !== "signed-in") jobs.push({ page: p, state: "signed-out" });
    if (p.kind !== "public" && !o.onlyPublic) {
      if (session) jobs.push({ page: p, state: "signed-in" });
      else if (p.kind === "signed-in") skipped.push(p.path);
    }
  }
  jobs.sort((a, b) =>
    a.state === b.state ? 0 : a.state === "signed-out" ? -1 : 1,
  );

  const started = new Date().toISOString();
  const browser = await chromium.launch({ headless: true });
  const contexts = new Map();
  const ctxFor = async (state, vp, scheme) => {
    const key = `${state} ${vp.name} ${scheme}`;
    if (contexts.has(key)) return contexts.get(key);
    const ctx = await browser.newContext({
      viewport: { width: vp.width, height: vp.height },
      isMobile: vp.mobile,
      hasTouch: vp.mobile,
      deviceScaleFactor: 1,
      colorScheme: scheme,
      reducedMotion: "reduce",
    });
    const origin = new URL(base).origin;
    await ctx.route("**/*", (route) =>
      new URL(route.request().url()).origin === origin
        ? route.continue()
        : route.abort("blockedbyclient"),
    );
    if (state === "signed-in")
      await ctx.addCookies([
        {
          name: "session",
          value: session,
          url: origin,
          httpOnly: true,
          sameSite: "Lax",
        },
      ]);
    contexts.set(key, ctx);
    return ctx;
  };

  const issues = new Issues();
  const results = [];
  const found = new Map();
  const known = new Map();
  let n = 0;
  const total = jobs.length * VIEWPORTS.length * SCHEMES.length;
  try {
    for (const job of jobs) {
      if (job.state === "signed-in" && local) local.ensureSession();
      const entry = {
        path: job.page.path,
        kind: job.page.kind,
        state: job.state,
        expected: job.page.status,
        shots: [],
      };
      for (const vp of VIEWPORTS) {
        for (const scheme of SCHEMES) {
          n++;
          const ctx = await ctxFor(job.state, vp, scheme);
          const s = await shoot(
            ctx,
            base,
            job.page,
            job.state,
            vp,
            scheme,
            outDir,
            issues,
          );
          log(
            `[${n}/${total}] ${job.page.path} ${job.state} ${vp.name} ${scheme}: ${s.status ?? s.error} ${s.ms ?? "-"} ms`,
          );
          for (const u of s.links || []) {
            const f = found.get(u) || { state: job.state, from: [] };
            if (!f.from.includes(job.page.path)) f.from.push(job.page.path);
            found.set(u, f);
          }
          const { links, ...rest } = s;
          rest.links = links ? links.length : 0;
          entry.shots.push(rest);
        }
      }
      known.set(job.page.path, entry.shots[0].status);
      results.push(entry);
    }
    const desktop = VIEWPORTS[1];
    const linkCtx = {
      "signed-out": await ctxFor("signed-out", desktop, "light"),
    };
    if (session && !o.onlyPublic)
      linkCtx["signed-in"] = await ctxFor("signed-in", desktop, "light");
    if (!o.pages) {
      await checkKeys(
        linkCtx["signed-out"],
        base,
        "signed-out",
        issues,
        outDir,
      );
      if (linkCtx["signed-in"]) {
        if (local) local.ensureSession();
        await checkKeys(
          linkCtx["signed-in"],
          base,
          "signed-in",
          issues,
          outDir,
        );
      }
    }
    const links = await checkLinks(
      found,
      (s) => linkCtx[s] || linkCtx["signed-out"],
      base,
      known,
      o.linkCap,
      issues,
    );
    const report = {
      base,
      mode: o.local ? "local" : "url",
      started,
      finished: new Date().toISOString(),
      pagesFile,
      viewports: VIEWPORTS.map((v) => v.name),
      schemes: SCHEMES,
      skipped,
      issues: issues.sorted(),
      pages: results,
      links,
    };
    fs.writeFileSync(
      path.join(outDir, "report.json"),
      `${JSON.stringify(report, null, 2)}\n`,
    );
    fs.writeFileSync(path.join(outDir, "index.html"), renderIndex(report));
    const c = { high: 0, medium: 0, low: 0 };
    for (const i of report.issues) c[i.severity]++;
    log(
      `\n${report.issues.length} issues: ${c.high} high, ${c.medium} medium, ${c.low} low`,
    );
    if (skipped.length)
      log(
        `${skipped.length} signed-in pages skipped: set MAINTAINER_MODE_AUDIT_SESSION or use --local`,
      );
    log(`open ${path.join(outDir, "index.html")}`);
  } finally {
    await browser.close();
    cleanup();
  }
}

main().catch((e) => {
  process.stderr.write(`ux-audit: ${e.stack || e}\n`);
  process.exit(1);
});
