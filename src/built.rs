//! Who built the pull request: the nixpkgs-review results that people post
//! on it. nixpkgs CI evaluates and lints a pull request; it does not build
//! the packages it changes. The only build evidence a pull request gets is
//! a person who ran nixpkgs-review on their own machine or in their own
//! GitHub Actions, and posted the result. Each such comment is evidence with
//! a name on it, like an approval.
//!
//! Two formats. nixpkgs-review itself, with `--post-result`, writes one
//! comment per system:
//!
//! ```text
//! Result of `nixpkgs-review pr 174272` run on x86_64-linux
//! <details><summary>1 package failed to build:</summary><ul><li>oraclejdk17</li></ul></details>
//! ```
//!
//! nixpkgs-review-gha (Defelo) writes one comment for every system, with
//! the commit it built, who triggered the run and a link to the logs:
//!
//! ```text
//! ## :x: `nixpkgs-review` result
//! Commit: [`3e336e6…`](…) ([subsequent changes](…))
//! Triggered by @alice (78835633)
//! Logs: https://github.com/alice/nixpkgs-review-gha/actions/runs/1
//! ### `x86_64-linux`
//! <details><summary>:white_check_mark: 1 package built:</summary><ul><li>entire</li></ul></details>
//! ### `aarch64-darwin` (sandbox = relaxed)
//! <details><summary>:x: 1 package failed to build:</summary><ul><li>entire</li></ul></details>
//! ```
//!
//! A run on an older head still counts, with a decay: half for each push
//! since, and nothing after [`MAX_PUSHES`].

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::model::{Pr, Run, System};

/// How many failed attributes a run keeps per system.
pub const FAILED_CAP: usize = 50;

/// A run this many pushes behind the head no longer counts.
pub const MAX_PUSHES: usize = 5;

/// The name of a run with no system named: the format before 2020-10.
pub const NO_SYSTEM: &str = "unknown system";

/// The nixpkgs-review result in a comment, or `None` when the comment is
/// not one. Loose on purpose: the format changed four times since 2019
/// and the fork adds lines of its own. A comment is a result when a line
/// names nixpkgs-review and a result; the systems come from "run on" or
/// from headings; the counts from "N packages built:" lines in any
/// wording that says built, failed or skipped; the names from `<li>` or
/// `-` items.
pub fn parse(body: &str, author: &str, url: &str, at: DateTime<Utc>) -> Option<Run> {
    let mut lines = body.lines();
    let header = loop {
        let l = lines.next()?;
        if is_header(l) {
            break l;
        }
    };
    let mut run = Run {
        by: author.to_string(),
        url: url.to_string(),
        at,
        commit: None,
        logs: None,
        systems: Vec::new(),
    };
    // "run on SYSTEM" names the one system of the comment; without it, a
    // heading names each one, or the whole comment is one unnamed system.
    let mut system = System::new(
        header
            .split(" run on ")
            .nth(1)
            .and_then(system_name)
            .unwrap_or_else(|| NO_SYSTEM.to_string()),
    );
    let mut kind: Option<Kind> = None;
    // the log tails of 2025 on: a build log can say "1 test failed:"
    let mut in_pre = false;
    for l in lines {
        let t = l.trim();
        if in_pre {
            in_pre = !t.contains("</pre>");
            continue;
        }
        if t.starts_with("<pre") {
            in_pre = !t.contains("</pre>");
            kind = None;
            continue;
        }
        if let Some(rest) = t.strip_prefix("Commit:") {
            run.commit = between(rest, '`', '`')
                .map(str::trim)
                .filter(|c| c.len() >= 7 && c.chars().all(|c| c.is_ascii_hexdigit()))
                .map(String::from);
            continue;
        }
        if let Some(rest) = t.strip_prefix("Logs:") {
            run.logs = rest
                .split_whitespace()
                .find(|w| w.starts_with("https://"))
                .map(|w| {
                    w.trim_matches(|c| c == '(' || c == ')' || c == '<' || c == '>')
                        .to_string()
                });
            continue;
        }
        if let Some(rest) = t.strip_prefix("Triggered by") {
            if let Some(login) = rest.split('@').nth(1).and_then(login_name) {
                run.by = login;
            }
            continue;
        }
        if let Some(name) = t
            .strip_prefix('#')
            .and_then(|h| system_name(h.trim_start_matches('#')))
        {
            if !system.is_empty() {
                run.systems.push(system);
            }
            system = System::new(name);
            kind = None;
            continue;
        }
        if t == "---" || t.starts_with("</details>") || t.starts_with("</ul>") {
            kind = None;
            continue;
        }
        let plain = strip(t);
        if let Some((n, k)) = summary(&plain) {
            kind = Some(k);
            match k {
                Kind::Built => system.built += n,
                Kind::Failed => system.failed_count += n,
                Kind::Skipped => system.skipped += n,
            }
            if k == Kind::Failed {
                for item in items_after_summary(t) {
                    system.push_failed(item);
                }
            }
            continue;
        }
        if kind == Some(Kind::Failed) {
            for item in items(t) {
                system.push_failed(item);
            }
        }
    }
    run.systems.push(system);
    run.systems.retain(|s| !s.is_empty());
    (!run.systems.is_empty()).then_some(run)
}

impl System {
    fn new(name: String) -> Self {
        System {
            name,
            built: 0,
            failed: Vec::new(),
            failed_count: 0,
            skipped: 0,
        }
    }

    fn is_empty(&self) -> bool {
        self.built + self.failed_count + self.skipped == 0
    }

    fn push_failed(&mut self, item: String) {
        if self.failed.len() < FAILED_CAP && !self.failed.contains(&item) {
            self.failed.push(item);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Built,
    Failed,
    Skipped,
}

/// `Result of \`nixpkgs-review pr N\`…` (2019 on), or `## \`nixpkgs-review\`
/// result` (2024 on, and the fork), in any markup.
fn is_header(l: &str) -> bool {
    let plain = strip(l).to_lowercase();
    plain.contains("nixpkgs-review") && plain.contains("result")
}

/// A Nix system, `x86_64-linux` or `aarch64-darwin`, at the start of the
/// text, with its markup and a tail such as `(sandbox = relaxed)` removed.
/// Every architecture has a digit in its name; `nixpkgs-review` has none.
fn system_name(s: &str) -> Option<String> {
    let s = strip(s);
    let word = s.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
    let name: String = word
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    let (arch, os) = name.split_once('-')?;
    (arch.chars().any(|c| c.is_ascii_digit())
        && !os.is_empty()
        && os.chars().all(|c| c.is_ascii_lowercase()))
    .then(|| name.clone())
}

fn login_name(s: &str) -> Option<String> {
    let login: String = s
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    (!login.is_empty()).then_some(login)
}

fn between(s: &str, open: char, close: char) -> Option<&str> {
    let start = s.find(open)? + open.len_utf8();
    let len = s[start..].find(close)?;
    Some(&s[start..start + len]).filter(|x| !x.is_empty())
}

/// The text with the HTML tags and the `:emoji:` codes removed.
fn strip(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace()
        .filter(|w| !(w.len() > 2 && w.starts_with(':') && w.ends_with(':')))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `N packages built:`, `N tests were built:`, `N package failed to
/// build:`, `N packages marked as broken and skipped:`, `N packages were
/// blacklisted:`, from a stripped line: the first number, then a word
/// that says what was counted, then what happened to them. An emoji
/// before the number is fine.
fn summary(plain: &str) -> Option<(u32, Kind)> {
    let mut words = plain.split_whitespace();
    let n: u32 = loop {
        let w = words.next()?;
        if let Ok(n) = w.parse() {
            break n;
        }
        if w.chars().any(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
    };
    let what = words.next()?;
    if !["package", "test", "derivation", "attribute", "build"]
        .iter()
        .any(|w| what.starts_with(w))
    {
        return None;
    }
    let rest: Vec<&str> = words.collect();
    let rest = rest.join(" ");
    let kind = if rest.contains("fail") {
        Kind::Failed
    } else if rest.contains("built") || rest.contains("succeeded") || rest.contains("passed") {
        Kind::Built
    } else if [
        "skip",
        "broken",
        "blacklist",
        "not found",
        "present in",
        "excluded",
        "ignored",
    ]
    .iter()
    .any(|w| rest.contains(w))
    {
        Kind::Skipped
    } else {
        return None;
    };
    Some((n, kind))
}

/// The list items after the `</summary>` of the same line.
fn items_after_summary(l: &str) -> Vec<String> {
    l.split_once("</summary>")
        .map(|(_, rest)| items(rest))
        .unwrap_or_default()
}

/// The names in `<li>…</li>` or a `- …` line.
fn items(l: &str) -> Vec<String> {
    let mut out = Vec::new();
    if l.contains("<li>") {
        for part in l.split("<li>").skip(1) {
            let item = part.split("</li>").next().unwrap_or(part);
            if let Some(name) = attr(item) {
                out.push(name);
            }
        }
    } else if let Some(rest) = l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")) {
        if let Some(name) = attr(rest) {
            out.push(name);
        }
    }
    out
}

/// The attribute at the start of a list item, with its markup removed:
/// `hello`, `hello (hello-unstable)`, `[hello](url)`, `` `hello` ``.
fn attr(item: &str) -> Option<String> {
    let s = strip(item);
    let s = s.trim().trim_start_matches('[').trim_matches('`');
    let name: String = s
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+'))
        .collect();
    (!name.is_empty()).then_some(name)
}

/// How many pushes reached the head after the run built it: 0 for the
/// current head. A run that names another commit is at least one behind,
/// even when the push list is capped.
pub fn pushes_since(run: &Run, pr: &Pr) -> usize {
    if run.commit.as_deref() == Some(pr.head_oid.as_str()) {
        return 0;
    }
    let n = pr.pushes.iter().filter(|p| **p > run.at).count();
    if run.commit.is_some() { n.max(1) } else { n }
}

/// 1 on the current head, half for each push since, 0 past [`MAX_PUSHES`].
pub fn weight(pushes_since: usize) -> f64 {
    if pushes_since > MAX_PUSHES {
        0.0
    } else {
        0.5f64.powi(pushes_since as i32)
    }
}

/// One run with its place against the head.
#[derive(Debug, Serialize)]
pub struct Aged<'a> {
    pub run: &'a Run,
    pub pushes_since: usize,
    pub weight: f64,
}

/// The runs that still count, the newest first.
pub fn aged(pr: &Pr) -> Vec<Aged<'_>> {
    let mut out: Vec<Aged> = pr
        .runs
        .iter()
        .map(|run| {
            let pushes_since = pushes_since(run, pr);
            Aged {
                run,
                pushes_since,
                weight: weight(pushes_since),
            }
        })
        .filter(|a| a.weight > 0.0)
        .collect();
    out.sort_by_key(|a| std::cmp::Reverse(a.run.at));
    out
}

/// The state of the build evidence of a pull request, for a list row: per
/// system, the freshest run decides.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Mark {
    /// The systems that built, and the systems with a failure.
    pub ok: Vec<String>,
    pub failed: Vec<String>,
    /// Who ran them.
    pub by: Vec<String>,
    /// The pushes since the oldest run used: 0 when all is on the head.
    pub pushes_since: usize,
    pub weight: f64,
}

pub fn mark(pr: &Pr) -> Option<Mark> {
    let aged = aged(pr);
    if aged.is_empty() {
        return None;
    }
    let mut ok = Vec::new();
    let mut failed = Vec::new();
    let mut by = Vec::new();
    let mut pushes_since = 0;
    // the freshest first: fewest pushes since, then the newest
    let mut order: Vec<&Aged> = aged.iter().collect();
    order.sort_by(|a, b| {
        a.pushes_since
            .cmp(&b.pushes_since)
            .then(b.run.at.cmp(&a.run.at))
    });
    for a in order {
        for s in &a.run.systems {
            if ok.contains(&s.name) || failed.contains(&s.name) {
                continue;
            }
            if s.failed_count > 0 {
                failed.push(s.name.clone());
            } else {
                ok.push(s.name.clone());
            }
            if !by.contains(&a.run.by) {
                by.push(a.run.by.clone());
            }
            pushes_since = pushes_since.max(a.pushes_since);
        }
    }
    Some(Mark {
        ok,
        failed,
        by,
        pushes_since,
        weight: weight(pushes_since),
    })
}

impl Mark {
    /// "built on 3 systems", "fails on aarch64-darwin", with the age.
    pub fn words(&self) -> String {
        let mut s = if self.failed.len() == 1 {
            format!("fails on {}", self.failed[0])
        } else if !self.failed.is_empty() {
            format!("fails on {} systems", self.failed.len())
        } else if self.ok.len() == 1 {
            format!("built on {}", self.ok[0])
        } else {
            format!("built on {} systems", self.ok.len())
        };
        s.push_str(&age_words(self.pushes_since));
        s
    }
}

/// "", " before the last push", " 3 pushes ago".
pub fn age_words(pushes_since: usize) -> String {
    match pushes_since {
        0 => String::new(),
        1 => " before the last push".into(),
        n => format!(" {n} pushes ago"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    const NATIVE_DETAILS: &str = "Result of `nixpkgs-review pr 174272` run on x86_64-linux [1](https://github.com/Mic92/nixpkgs-review)
<details>
  <summary>1 package failed to build:</summary>
  <ul>
    <li>oraclejdk17</li>
  </ul>
</details>
<details>
  <summary>2 packages built:</summary>
  <ul>
    <li>hello</li>
    <li>jq</li>
  </ul>
</details>";

    const NATIVE_PLAIN: &str = "Result of `nixpkgs-review pr 577185` run on aarch64-darwin

1 package built:
- entire

Both `entire` and `git-remote-entire` land in `result/bin`.";

    const GHA: &str = "## :x: `nixpkgs-review` result

Generated using [`nixpkgs-review-gha`](https://github.com/Defelo/nixpkgs-review-gha) ([`af6a071`](https://github.com/Defelo/nixpkgs-review-gha/commit/af6a071))

Command: `nixpkgs-review pr 577185`
Commit: [`3e336e619f02175f6141a1494da70af356d68080`](https://github.com/NixOS/nixpkgs/commit/3e336e619f02175f6141a1494da70af356d68080) ([subsequent changes](https://github.com/NixOS/nixpkgs/compare/3e336e6..pull/577185/head))
Merge: [`19289ee910bcef158d3dd3ed369a9795a83c92b7`](https://github.com/NixOS/nixpkgs/commit/19289ee9)

Triggered by @iamanaws (78835633)
Logs: https://github.com/iamanaws/nixpkgs-review-gha/actions/runs/40382153678/attempts/1

<details><summary>Download packages from cache:</summary><ul>
<li><details><summary><code>x86_64-linux</code></summary>

```shell
nix-store -r /nix/store/abc-entire
```
</details></li>
</ul></details>

---
### `x86_64-linux`
<details><summary>:white_check_mark: 1 package built:</summary><ul>
<li>entire</li>
</ul></details>

---
### `aarch64-linux`
<details><summary>:white_check_mark: 1 package built:</summary><ul>
<li>entire</li>
</ul></details>

---
### `aarch64-darwin` (sandbox = relaxed)
<details><summary>:x: 1 package failed to build:</summary><ul>
<li>entire</li>
</ul></details>
<details><summary>:warning: 1 package marked as broken and skipped:</summary><ul>
<li>old-thing</li>
</ul></details>";

    const GHA_OLD: &str = "## `nixpkgs-review` result

Generated using [`nixpkgs-review-gha`](https://github.com/Defelo/nixpkgs-review-gha)

Command: `nixpkgs-review pr 431108`

Logs: https://github.com/yzhou216/nixpkgs-review-gha/actions/runs/16745317095


---
### `x86_64-linux`
<details>
  <summary>:white_check_mark: 1 package built:</summary>
  <ul>
    <li>cargo-doc2readme</li>
  </ul>
</details>
";

    fn t0() -> DateTime<Utc> {
        at("2026-10-26T19:01:24Z")
    }

    #[test]
    fn the_native_format_parses_with_and_without_markup() {
        let r = parse(NATIVE_DETAILS, "bachp", "u", t0()).unwrap();
        assert_eq!(r.by, "bachp");
        assert_eq!(r.commit, None);
        assert_eq!(r.systems.len(), 1);
        let s = &r.systems[0];
        assert_eq!(s.name, "x86_64-linux");
        assert_eq!((s.built, s.failed_count, s.skipped), (2, 1, 0));
        assert_eq!(s.failed, vec!["oraclejdk17"]);

        let r = parse(NATIVE_PLAIN, "toothbrush", "u", t0()).unwrap();
        let s = &r.systems[0];
        assert_eq!(s.name, "aarch64-darwin");
        assert_eq!((s.built, s.failed_count), (1, 0));
        assert!(s.failed.is_empty());
    }

    #[test]
    fn the_gha_format_names_the_person_the_commit_and_each_system() {
        let r = parse(GHA, "nixpkgs-review-gha", "u", t0()).unwrap();
        assert_eq!(r.by, "iamanaws");
        assert_eq!(
            r.commit.as_deref(),
            Some("3e336e619f02175f6141a1494da70af356d68080")
        );
        assert_eq!(
            r.logs.as_deref(),
            Some(
                "https://github.com/iamanaws/nixpkgs-review-gha/actions/runs/40382153678/attempts/1"
            )
        );
        let names: Vec<&str> = r.systems.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["x86_64-linux", "aarch64-linux", "aarch64-darwin"]);
        assert_eq!(r.systems[0].built, 1);
        let d = &r.systems[2];
        assert_eq!((d.built, d.failed_count, d.skipped), (0, 1, 1));
        assert_eq!(d.failed, vec!["entire"]);

        let r = parse(GHA_OLD, "yzhou216", "u", t0()).unwrap();
        assert_eq!(r.by, "yzhou216");
        assert_eq!(r.commit, None);
        assert_eq!(r.systems.len(), 1);
        assert_eq!(r.systems[0].built, 1);
    }

    /// Real comments of NixOS/nixpkgs #577185, #431108 and #174272, read
    /// on 2026-10-28, with the cachix commands cut out.
    #[test]
    fn the_real_comments_parse() {
        let items: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("testdata/built.json")).unwrap();
        let runs: Vec<Run> = items
            .iter()
            .map(|c| {
                parse(
                    c["body"].as_str().unwrap(),
                    c["author"].as_str().unwrap(),
                    c["url"].as_str().unwrap(),
                    at(c["created_at"].as_str().unwrap()),
                )
                .unwrap_or_else(|| panic!("{}", c["url"]))
            })
            .collect();
        assert_eq!(runs.len(), 6);
        let brief: Vec<String> = runs
            .iter()
            .map(|r| {
                format!(
                    "{} {} {}",
                    r.by,
                    r.commit.as_deref().map(|c| &c[..7]).unwrap_or("-"),
                    r.systems
                        .iter()
                        .map(|s| format!("{}:{}/{}/{}", s.name, s.built, s.failed_count, s.skipped))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            })
            .collect();
        assert_eq!(
            brief,
            [
                "toothbrush - aarch64-darwin:1/0/0",
                "iamanaws 3e336e6 x86_64-linux:1/0/0,aarch64-linux:1/0/0,aarch64-darwin:0/1/0",
                "iamanaws 9e649a7 x86_64-linux:1/0/0,aarch64-linux:1/0/0,aarch64-darwin:1/0/0",
                "yzhou216 - x86_64-linux:1/0/0,aarch64-linux:1/0/0,x86_64-darwin:1/0/0,aarch64-darwin:1/0/0",
                "yzhou216 be04848 x86_64-linux:1/0/0,aarch64-linux:1/0/0,aarch64-darwin:1/0/0",
                "bachp - x86_64-linux:0/1/0",
            ]
        );
        assert_eq!(runs[1].systems[2].failed, ["entire"]);
        assert_eq!(runs[5].systems[0].failed, ["oraclejdk17"]);
        assert!(
            runs[1]
                .logs
                .as_deref()
                .unwrap()
                .contains("/actions/runs/40382153678")
        );
    }

    /// The format of 2019-12 to 2020-10: no system, `-` items, "were".
    const V2019: &str =
        "Result of `nixpkgs-review pr 75000` [1](https://github.com/Mic92/nixpkgs-review)
<details>
  <summary>1 packages are marked as broken and were skipped:</summary>

  - old-thing
</details>
<details>
  <summary>2 packages failed to build:</summary>

  - hello (hello-unstable)
  - jq
</details>
<details>
  <summary>1 tests were built:</summary>

  - nixosTests.hello
</details>
<details>
  <summary>3 packages were built:</summary>

  - a
  - b
  - c
</details>
";

    /// The format of 2024-09 on, by nixpkgs-review itself: unicode emoji,
    /// a plain commit, aliases, tests, and the log tails of 2025-05 on.
    const V2024: &str = "## `nixpkgs-review` result

Generated using [`nixpkgs-review`](https://github.com/Mic92/nixpkgs-review).

Command: `nixpkgs-review pr 600000 --checkout commit`
Commit: `abcdef0123456789abcdef0123456789abcdef01`

---
### `x86_64-linux`
<details>
  <summary>⏩ 1 package present in ofBorg's evaluation, but not found in the checkout:</summary>
  <ul>
    <li>gone</li>
  </ul>
</details>
<details>
  <summary>❌ 1 package failed to build:</summary>
  <ul>
    <li>hello (hello-unstable, hello_2)</li>
  </ul>
</details>
<details>
  <summary>✅ 2 tests built:</summary>
  <ul>
    <li>nixosTests.hello</li>
    <li>hello.passthru.tests.version</li>
  </ul>
</details>
<details>
  <summary>✅ 1 package built:</summary>
  <ul>
    <li>jq</li>
  </ul>
</details>

---
<details>
<summary>hello</summary>
<pre>make: *** [Makefile:12: all] Error 1
1 test failed:
- the one that matters
</pre>
</details>

---
### `aarch64-linux`
<details>
  <summary>✅ 1 package built:</summary>
  <ul>
    <li>hello</li>
  </ul>
</details>
";

    #[test]
    fn the_older_formats_parse_too() {
        let r = parse(V2019, "mic92", "u", t0()).unwrap();
        assert_eq!(r.systems.len(), 1);
        let s = &r.systems[0];
        assert_eq!(s.name, NO_SYSTEM);
        assert_eq!(
            (s.built, s.failed_count, s.skipped),
            (4, 2, 1),
            "tests count as built"
        );
        assert_eq!(s.failed, vec!["hello", "jq"], "the alias is not the name");

        let r = parse(V2024, "mic92", "u", t0()).unwrap();
        assert_eq!(
            r.commit.as_deref(),
            Some("abcdef0123456789abcdef0123456789abcdef01")
        );
        assert_eq!(r.systems.len(), 2, "the log tail is not a system");
        let s = &r.systems[0];
        assert_eq!(s.name, "x86_64-linux");
        assert_eq!((s.built, s.failed_count, s.skipped), (3, 1, 1));
        assert_eq!(s.failed, vec!["hello"], "nothing from the log tail");
        assert_eq!(r.systems[1].name, "aarch64-linux");
        assert_eq!(r.systems[1].built, 1);
    }

    #[test]
    fn a_system_needs_a_digit_in_its_architecture() {
        assert_eq!(
            system_name("`x86_64-linux` (sandbox = relaxed)").as_deref(),
            Some("x86_64-linux")
        );
        assert_eq!(
            system_name(" aarch64-darwin").as_deref(),
            Some("aarch64-darwin")
        );
        assert_eq!(
            system_name("riscv64-linux [1](x)").as_deref(),
            Some("riscv64-linux")
        );
        assert_eq!(system_name("`nixpkgs-review` result"), None);
        assert_eq!(system_name("Summary"), None);
    }

    #[test]
    fn other_comments_are_not_runs() {
        assert!(parse("LGTM, I ran nixpkgs-review and it works", "a", "u", t0()).is_none());
        assert!(
            parse(
                "Result of `nixpkgs-review pr 1` run on x86_64-linux",
                "a",
                "u",
                t0()
            )
            .is_none()
        );
        assert!(parse("", "a", "u", t0()).is_none());
    }

    #[test]
    fn the_failed_list_is_capped() {
        let mut body = String::from(
            "Result of `nixpkgs-review pr 1` run on x86_64-linux\n<details><summary>60 packages failed to build:</summary><ul>\n",
        );
        for i in 0..60 {
            body.push_str(&format!("<li>pkg{i}</li>\n"));
        }
        let r = parse(&body, "a", "u", t0()).unwrap();
        assert_eq!(r.systems[0].failed_count, 60);
        assert_eq!(r.systems[0].failed.len(), FAILED_CAP);
    }

    fn pr_with(runs: Vec<Run>, pushes: &[&str]) -> Pr {
        let mut p = crate::ledger::tests::pr(1, "alice", 0);
        p.head_oid = "head".into();
        p.pushes = pushes.iter().map(|s| at(s)).collect();
        p.runs = runs;
        p
    }

    fn run(by: &str, when: &str, commit: Option<&str>, systems: &[(&str, u32)]) -> Run {
        Run {
            by: by.into(),
            url: format!("https://example/{by}"),
            at: at(when),
            commit: commit.map(String::from),
            logs: None,
            systems: systems
                .iter()
                .map(|(n, failed)| System {
                    name: n.to_string(),
                    built: 1,
                    failed: (0..*failed).map(|i| format!("f{i}")).collect(),
                    failed_count: *failed,
                    skipped: 0,
                })
                .collect(),
        }
    }

    #[test]
    fn a_run_decays_by_the_pushes_since_it() {
        let pushes = ["2026-10-01T00:00:00Z", "2026-10-05T00:00:00Z"];
        let p = pr_with(vec![], &pushes);
        let on_head = run("a", "2026-10-02T00:00:00Z", Some("head"), &[]);
        assert_eq!(pushes_since(&on_head, &p), 0);
        let other = run("a", "2026-10-06T00:00:00Z", Some("older"), &[]);
        assert_eq!(
            pushes_since(&other, &p),
            1,
            "names another commit, so at least one"
        );
        let unnamed = run("a", "2026-09-30T00:00:00Z", None, &[]);
        assert_eq!(pushes_since(&unnamed, &p), 2);
        let fresh = run("a", "2026-10-06T00:00:00Z", None, &[]);
        assert_eq!(pushes_since(&fresh, &p), 0);
        assert_eq!(weight(0), 1.0);
        assert_eq!(weight(1), 0.5);
        assert_eq!(weight(3), 0.125);
        assert_eq!(weight(MAX_PUSHES + 1), 0.0);
    }

    #[test]
    fn the_mark_takes_the_freshest_run_per_system() {
        let pushes = ["2026-10-05T00:00:00Z"];
        let old = run(
            "bob",
            "2026-10-01T00:00:00Z",
            None,
            &[("x86_64-linux", 1), ("aarch64-darwin", 0)],
        );
        let new = run(
            "alice",
            "2026-10-06T00:00:00Z",
            Some("head"),
            &[("x86_64-linux", 0)],
        );
        let p = pr_with(vec![old, new], &pushes);
        let m = mark(&p).unwrap();
        assert_eq!(m.ok, ["x86_64-linux", "aarch64-darwin"]);
        assert!(m.failed.is_empty());
        assert_eq!(m.by, ["alice", "bob"]);
        assert_eq!(
            m.pushes_since, 1,
            "the darwin result is from before the push"
        );
        assert_eq!(m.words(), "built on 2 systems before the last push");

        let failing = run(
            "carol",
            "2026-10-07T00:00:00Z",
            Some("head"),
            &[("aarch64-darwin", 2)],
        );
        let p = pr_with(vec![failing], &pushes);
        assert_eq!(mark(&p).unwrap().words(), "fails on aarch64-darwin");
        assert!(mark(&pr_with(vec![], &pushes)).is_none());

        let stale = run("dan", "2026-01-01T00:00:00Z", None, &[("x86_64-linux", 0)]);
        let many: Vec<&str> = vec!["2026-02-01T00:00:00Z"; MAX_PUSHES + 1];
        assert!(
            mark(&pr_with(vec![stale], &many)).is_none(),
            "too many pushes since"
        );
    }
}
