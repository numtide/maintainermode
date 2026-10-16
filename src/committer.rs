//! Who has merge rights, from public data, and the view for them.

use anyhow::{Result, bail};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use std::collections::HashSet;

use crate::model::Pr;
use crate::store::Store;

pub const TEAM: &str = "NixOS/nixpkgs-committers";
const MERGED_DAYS: i64 = 365;

/// Logins are kept in lower case: `meta.maintainers` and GitHub disagree on case.
#[derive(Debug, Default)]
pub struct Committers {
    logins: HashSet<String>,
    pub from_merges: usize,
    pub from_team: usize,
    pub team_at: Option<DateTime<Utc>>,
}

impl Committers {
    pub fn build(
        prs: &[Pr],
        team: &[String],
        team_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Self {
        let since = now - Duration::days(MERGED_DAYS);
        let merged: HashSet<String> = prs
            .iter()
            .filter(|p| p.merged_at.is_some_and(|t| t >= since))
            .filter_map(|p| p.merged_by.as_deref())
            .map(str::to_lowercase)
            .collect();
        let team: HashSet<String> = team.iter().map(|l| l.to_lowercase()).collect();
        Committers {
            from_merges: merged.len(),
            from_team: team.len(),
            logins: merged.union(&team).cloned().collect(),
            team_at,
        }
    }

    pub fn load(store: &Store, prs: &[Pr], now: DateTime<Utc>) -> Self {
        let team: Vec<String> = store
            .get("committers_team")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let at = store.get_time("committers_team_read").ok().flatten();
        Self::build(prs, &team, at, now)
    }

    pub fn is(&self, login: &str) -> bool {
        self.logins.contains(&login.to_lowercase())
    }

    pub fn len(&self) -> usize {
        self.logins.len()
    }
}

/// Reads the team once a day with the server's own tokens. A token that may
/// not see the team is normal: the merges in the synced data still count.
pub async fn refresh_team(store: &Store, tokens: Vec<String>) -> Result<()> {
    let tried = store.get_time("committers_team_tried")?;
    if tokens.is_empty() || tried.is_some_and(|t| Utc::now() - t < Duration::hours(24)) {
        return Ok(());
    }
    store.set("committers_team_tried", &Utc::now().to_rfc3339())?;
    let (org, slug) = TEAM.split_once('/').unwrap();
    let query = "query($org: String!, $slug: String!, $after: String) { rateLimit { remaining }
        organization(login: $org) { team(slug: $slug) {
          members(first: 100, after: $after, membership: ALL) { nodes { login } pageInfo { hasNextPage endCursor } } } } }";
    let mut client = crate::github::Client::new(tokens);
    let mut after = serde_json::Value::Null;
    let mut logins = Vec::new();
    for _ in 0..50 {
        let data = client
            .graphql(
                store,
                query,
                json!({ "org": org, "slug": slug, "after": after }),
            )
            .await?;
        let members = &data["organization"]["team"]["members"];
        if members.is_null() {
            bail!("{TEAM} is not visible to the server token");
        }
        logins.extend(
            members["nodes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|n| n["login"].as_str())
                .map(str::to_string),
        );
        if members["pageInfo"]["hasNextPage"] != true {
            store.set("committers_team", &serde_json::to_string(&logins)?)?;
            store.set("committers_team_read", &Utc::now().to_rfc3339())?;
            return Ok(());
        }
        after = members["pageInfo"]["endCursor"].clone();
    }
    bail!("{TEAM}: too many pages")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::tests::{pr, t};

    #[test]
    fn a_recent_merge_or_the_team_gives_merge_rights() {
        let mut old = pr(1, "alice", 0);
        old.merged_at = Some(t(0));
        old.merged_by = Some("Olda".into());
        let mut new = pr(2, "alice", 300);
        new.merged_at = Some(t(300));
        new.merged_by = Some("Newb".into());
        let c = Committers::build(&[old, new], &["Teamy".into()], None, t(400));
        assert!(c.is("newb"));
        assert!(c.is("TEAMY"));
        assert!(!c.is("olda"));
        assert!(!c.is("alice"));
        assert_eq!((c.from_merges, c.from_team, c.len()), (1, 1, 2));
    }
}
