use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::tools::optional_usize;

pub(super) fn fingerprint(value: impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// A page over a deterministic ordering. Tokens are bound to the loaded
/// artifact and the effective query so they can be validated without a session.
pub(super) struct Page {
    total: usize,
    start: usize,
    end: usize,
    query: u64,
}

impl Page {
    pub(super) fn new(
        args: &Value,
        default_limit: usize,
        total: usize,
        manifest_hash: u64,
        query: impl Hash,
    ) -> Result<Self> {
        let limit = optional_usize(args, "limit")?.unwrap_or(default_limit);
        ensure!(
            (1..=100).contains(&limit),
            "limit must be between 1 and 100"
        );
        let query = fingerprint((manifest_hash, query));
        let start = match args.get("cursor") {
            None | Some(Value::Null) => 0,
            Some(value) => {
                let token = value.as_str().context("cursor must be a string")?;
                let (token, offset) = token.split_once('.').context("invalid cursor")?;
                let token = u64::from_str_radix(token, 16).context("invalid cursor")?;
                let offset = usize::from_str_radix(offset, 16).context("invalid cursor")?;
                ensure!(
                    token == query && offset > 0 && offset < total,
                    "cursor does not match this manifest or query"
                );
                offset
            }
        };
        let end = start.saturating_add(limit).min(total);
        Ok(Self {
            total,
            start,
            end,
            query,
        })
    }

    pub(super) fn range(&self) -> Range<usize> {
        self.start..self.end
    }

    pub(super) fn metadata(&self) -> Value {
        json!({
            "total": self.total,
            "returned": self.end - self.start,
            "remaining": self.total - self.end,
            "next_cursor": (self.end < self.total).then(|| format!("{:016x}.{:x}", self.query, self.end)),
        })
    }
}
