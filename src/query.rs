//! The search box as a query, as GitHub's and nixkeeper's: qualifiers
//! such as `is:outdated -has:pr by:zimbatm` beside the words of a name. The
//! grammar is nixkeeper's (`page/search.js`), so a query written there
//! works here: a token is `-key:value` or `-key:"a value with spaces"`,
//! the key in any case; a word with no colon, or an `@handle`, is text.
//!
//! This module only splits the query. What each qualifier means is the
//! page's: see [`crate::catalog::filter`] for packages.

use std::fmt::Write;

/// One qualifier of a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Term {
    /// `-key:value` leaves the value out.
    pub neg: bool,
    /// Lowercase.
    pub key: String,
    /// Without its quotes.
    pub value: String,
    /// As typed, for a message about it.
    pub raw: String,
}

impl Term {
    /// `-is:broken`: the term with its sign, as a word of the grammar.
    pub(crate) fn word(&self) -> String {
        let mut out = String::new();
        if self.neg {
            out.push('-');
        }
        let _ = write!(out, "{}:{}", self.key, self.value);
        out
    }
}

/// A query split into its qualifiers and the rest.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Query {
    pub terms: Vec<Term>,
    /// The words that are not qualifiers, with one space between them.
    pub text: String,
}

/// A word the page does not know, and what was likely meant.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Problem {
    pub token: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

impl Problem {
    pub(crate) fn new(token: &str, message: impl Into<String>) -> Problem {
        Problem {
            token: token.to_string(),
            message: message.into(),
            suggestion: None,
        }
    }

    /// A key the page does not know: the closest of `keys`, as the whole
    /// word with the sign and the value kept, so it can replace the token.
    pub(crate) fn of_key<'a>(
        t: &Term,
        message: impl Into<String>,
        keys: impl IntoIterator<Item = &'a str>,
    ) -> Problem {
        let mut p = Problem::new(&t.raw, message);
        p.suggestion = closest(&t.key, keys).map(|k| {
            Term {
                key: k.to_string(),
                ..t.clone()
            }
            .word()
        });
        p
    }

    /// A value the page does not know: the closest of `values`, as the
    /// whole word under the same key.
    pub(crate) fn of_value<'a>(
        t: &Term,
        message: impl Into<String>,
        values: impl IntoIterator<Item = &'a str>,
    ) -> Problem {
        let mut p = Problem::new(&t.raw, message);
        p.suggestion = closest(&t.value, values).map(|v| {
            Term {
                value: v.to_string(),
                ..t.clone()
            }
            .word()
        });
        p
    }

    /// With the closest of `words` to `as_typed`, when one is close; the
    /// words are whole tokens.
    pub(crate) fn suggest<'a>(
        mut self,
        as_typed: &str,
        words: impl IntoIterator<Item = &'a str>,
    ) -> Problem {
        self.suggestion = closest(as_typed, words).map(String::from);
        self
    }
}

/// The tokens of `q`: a quoted qualifier value keeps its spaces.
fn tokens(q: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = q.trim();
    while !rest.is_empty() {
        let end = match quoted_end(rest) {
            Some(e) => e,
            None => rest.find(char::is_whitespace).unwrap_or(rest.len()),
        };
        out.push(rest[..end].to_string());
        rest = rest[end..].trim_start();
    }
    out
}

/// Where a token `-?key:"..."` ends, when `s` starts with one.
fn quoted_end(s: &str) -> Option<usize> {
    let body = s.strip_prefix('-').unwrap_or(s);
    let colon = body.find(':')?;
    let key = &body[..colon];
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let after = &body[colon + 1..];
    let inner = after.strip_prefix('"')?;
    let close = inner.find('"')?;
    Some(s.len() - after.len() + 1 + close + 1)
}

pub(crate) fn parse(q: &str) -> Query {
    let mut out = Query::default();
    let mut text: Vec<String> = Vec::new();
    for tok in tokens(q) {
        if tok.starts_with('@') {
            text.push(tok);
            continue;
        }
        let (neg, body) = match tok.strip_prefix('-') {
            Some(b) => (true, b),
            None => (false, tok.as_str()),
        };
        let Some((key, value)) = body.split_once(':') else {
            text.push(tok);
            continue;
        };
        // `https://…` is a link, not a qualifier.
        if key.is_empty()
            || !key.chars().all(|c| c.is_ascii_alphabetic())
            || value.starts_with("//")
        {
            text.push(tok);
            continue;
        }
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value);
        out.terms.push(Term {
            neg,
            key: key.to_ascii_lowercase(),
            value: value.to_string(),
            raw: tok.clone(),
        });
    }
    out.text = text.join(" ");
    out
}

fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let sub = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + sub);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The closest of `words` to `word`, when it is within a third of the
/// length of `word` (one edit at least), as nixkeeper suggests.
pub(crate) fn closest<'a>(word: &str, words: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let w = word.to_lowercase();
    let allowed = (word.chars().count() / 3).max(1);
    words
        .into_iter()
        .map(|c| (distance(&w, &c.to_lowercase()), c))
        .filter(|(d, _)| *d <= allowed)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(neg: bool, key: &str, value: &str, raw: &str) -> Term {
        Term {
            neg,
            key: key.into(),
            value: value.into(),
            raw: raw.into(),
        }
    }

    #[test]
    fn qualifiers_beside_a_name() {
        let q = parse("is:outdated -has:pr  by:@Alice wine");
        assert_eq!(q.text, "wine");
        assert_eq!(
            q.terms,
            [
                term(false, "is", "outdated", "is:outdated"),
                term(true, "has", "pr", "-has:pr"),
                term(false, "by", "@Alice", "by:@Alice"),
            ]
        );
    }

    #[test]
    fn a_quoted_value_keeps_its_spaces() {
        let q = parse("-team:\"Security review\" hello IS:Broken");
        assert_eq!(q.text, "hello");
        assert_eq!(
            q.terms[0],
            term(true, "team", "Security review", "-team:\"Security review\"")
        );
        assert_eq!(q.terms[1], term(false, "is", "Broken", "IS:Broken"));
        assert_eq!(q.terms[1].word(), "is:Broken");
    }

    #[test]
    fn what_is_not_a_qualifier_is_text() {
        let q = parse("@zimbatm nixos/nginx pkgs/by-name https://x.y/z 2024:1");
        assert!(q.terms.is_empty());
        assert_eq!(
            q.text,
            "@zimbatm nixos/nginx pkgs/by-name https://x.y/z 2024:1"
        );
        assert_eq!(parse("").text, "");
        assert_eq!(parse("  hel  ").text, "hel");
    }

    #[test]
    fn the_closest_word_within_a_third() {
        let words = ["outdated", "vulnerable", "broken", "failed"];
        assert_eq!(closest("outdatd", words), Some("outdated"));
        assert_eq!(closest("Broken", words), Some("broken"));
        assert_eq!(closest("xyz", words), None);
        assert_eq!(closest("vulnrable", words), Some("vulnerable"));
    }
}
