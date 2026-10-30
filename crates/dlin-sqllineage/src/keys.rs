//! Dialect-aware identity for collections that retain their original spelling.

use crate::types::TableRef;
use crate::{Candidate, Dialect, Ident, Relation, SourceColumn, Unresolved};

type RelationKey = (Option<String>, Option<String>, String);

fn parts_key(
    catalog: Option<&Ident>,
    schema: Option<&Ident>,
    name: &Ident,
    dialect: Dialect,
) -> RelationKey {
    (
        catalog.map(|id| id.relation_key(dialect).into_owned()),
        schema.map(|id| id.relation_key(dialect).into_owned()),
        name.relation_key(dialect).into_owned(),
    )
}

pub(crate) fn relation_key(relation: &Relation, dialect: Dialect) -> RelationKey {
    parts_key(
        relation.catalog.as_ref(),
        relation.schema.as_ref(),
        &relation.name,
        dialect,
    )
}

pub(crate) fn table_key(table: &TableRef, dialect: Dialect) -> RelationKey {
    parts_key(
        table.catalog.as_ref(),
        table.schema.as_ref(),
        &table.table,
        dialect,
    )
}

pub(crate) fn source_key(source: &SourceColumn, dialect: Dialect) -> (RelationKey, String) {
    (
        relation_key(&source.relation, dialect),
        source.column.column_key(dialect).into_owned(),
    )
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CandidateKey {
    Relation(RelationKey),
    Cte(String),
    Derived(Option<String>),
}

pub(crate) fn candidate_key(candidate: &Candidate, dialect: Dialect) -> CandidateKey {
    match candidate {
        Candidate::Relation(relation) => CandidateKey::Relation(relation_key(relation, dialect)),
        Candidate::Cte(name) => CandidateKey::Cte(name.relation_key(dialect).into_owned()),
        Candidate::Derived(name) => CandidateKey::Derived(
            name.as_ref()
                .map(|id| id.relation_key(dialect).into_owned()),
        ),
    }
}

/// Stable sorting keeps the first spelling among equal SQL keys.
pub(crate) fn sort_dedup_by_key<T, K: Ord>(values: &mut Vec<T>, mut key: impl FnMut(&T) -> K) {
    values.sort_by_cached_key(|value| key(value));
    values.dedup_by(|later, earlier| key(later) == key(earlier));
}

pub(crate) fn ident_parts_equal(left: &[Ident], right: &[Ident], dialect: Dialect) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.relation_key(dialect) == right.relation_key(dialect))
}

#[derive(PartialEq, Eq)]
pub(crate) enum UnresolvedKey {
    BehindStar(RelationKey),
    NamedBehindStar(RelationKey, String),
    Ambiguous(String, Vec<CandidateKey>),
    Unbound(String),
    Recursive,
    RowValue(String),
    Unsupported(&'static str),
}

pub(crate) fn unresolved_key(unresolved: &Unresolved, dialect: Dialect) -> UnresolvedKey {
    match unresolved {
        Unresolved::BehindStar { relation } => {
            UnresolvedKey::BehindStar(relation_key(relation, dialect))
        }
        Unresolved::NamedBehindStar { relation, column } => UnresolvedKey::NamedBehindStar(
            relation_key(relation, dialect),
            column.column_key(dialect).into_owned(),
        ),
        Unresolved::Ambiguous { column, candidates } => UnresolvedKey::Ambiguous(
            column.column_key(dialect).into_owned(),
            candidates
                .iter()
                .map(|candidate| candidate_key(candidate, dialect))
                .collect(),
        ),
        Unresolved::Unbound { column } => {
            UnresolvedKey::Unbound(column.column_key(dialect).into_owned())
        }
        Unresolved::Recursive => UnresolvedKey::Recursive,
        Unresolved::RowValue { alias } => {
            UnresolvedKey::RowValue(alias.relation_key(dialect).into_owned())
        }
        Unresolved::Unsupported { what } => UnresolvedKey::Unsupported(what),
    }
}
