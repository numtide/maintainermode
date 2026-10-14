use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The facts about one pull request that the ledger reads. Every field comes
/// from public GitHub data; nothing here is entered by hand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pr {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub author_is_bot: bool,
    /// OPEN, CLOSED or MERGED.
    pub state: String,
    pub is_draft: bool,
    pub base: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub merged_at: Option<DateTime<Utc>>,
    /// MERGEABLE, CONFLICTING or UNKNOWN.
    pub mergeable: String,
    pub head_oid: String,
    /// Paths the pull request changes, capped at the first 50.
    pub files: Vec<String>,
    /// The status rollup of the head commit: SUCCESS, FAILURE, PENDING, ERROR.
    pub ci: Option<String>,
    pub reviews: Vec<Review>,
    /// When the author commented on the pull request or replied in a thread.
    pub author_activity: Vec<DateTime<Utc>>,
    /// When new commits reached the head, by commit date or force push.
    pub pushes: Vec<DateTime<Utc>>,
    pub voids: Vec<Void>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    pub url: String,
    pub author: String,
    pub author_is_bot: bool,
    /// APPROVED, CHANGES_REQUESTED, COMMENTED or DISMISSED.
    pub state: String,
    pub submitted_at: DateTime<Utc>,
    pub commit_oid: Option<String>,
    /// The review carries an inline comment whose line is a `+` or `-` line.
    pub inline_on_change: bool,
}

/// A committer's public statement, in a comment on the pull request, that one
/// review does not count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Void {
    pub review_id: String,
    pub by: String,
    pub url: String,
    pub at: DateTime<Utc>,
}

impl Review {
    /// The numeric id after `#pullrequestreview-`, which is how a void names it.
    pub fn id(&self) -> &str {
        self.url
            .rsplit_once("#pullrequestreview-")
            .map(|(_, id)| id)
            .unwrap_or("")
    }
}

impl Pr {
    pub fn is_open(&self) -> bool {
        self.state == "OPEN"
    }
}
