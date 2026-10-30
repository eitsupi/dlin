//! Dialect-aware identity for collections that retain their original spelling.

use crate::{Dialect, Ident};

pub(crate) fn ident_parts_equal(left: &[Ident], right: &[Ident], dialect: Dialect) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.relation_key(dialect) == right.relation_key(dialect))
}
