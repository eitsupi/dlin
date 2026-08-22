#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use super::super::relation::{RelationRef, RelationResolution};
use super::dialect::DlinDialect;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogSnapshot {
    /// Physical relations are keyed by their structural identity. Lookup
    /// aliases are kept separately so an alias collision cannot overwrite a
    /// different physical relation.
    tables: BTreeMap<RelationRef, CatalogTable>,
    aliases: BTreeMap<RelationRef, BTreeSet<RelationRef>>,
    /// Compatibility views for backend adapters that still consume the
    /// legacy string-facing `tables()` iterator. These are never identity
    /// storage; their `relation` points back to the physical entry.
    alias_views: Vec<CatalogTable>,
    conflicted: BTreeSet<RelationRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogTable {
    /// Preferred display spelling, normally derived from the manifest.
    pub name: String,
    pub columns: Vec<String>,
    pub(crate) relation: RelationRef,
}

impl CatalogSnapshot {
    pub fn new() -> Self {
        Self {
            tables: BTreeMap::new(),
            aliases: BTreeMap::new(),
            alias_views: Vec::new(),
            conflicted: BTreeSet::new(),
        }
    }

    /// Compatibility entry point for callers that only have a relation
    /// string. New manifest code should use [`Self::add_relation`] and
    /// [`Self::add_alias`] so dots inside an identifier remain one component.
    pub fn add_table(
        &mut self,
        name: impl Into<String>,
        columns: impl IntoIterator<Item = String>,
    ) {
        let name = name.into();
        let relation = RelationRef::parse(&name)
            .map(|relation| relation.as_manifest())
            .unwrap_or_else(|_| RelationRef::bare(&name).as_manifest());
        self.add_relation(relation, name, columns);
    }

    /// Register one physical relation and its columns.
    pub(crate) fn add_relation(
        &mut self,
        relation: RelationRef,
        preferred_name: impl Into<String>,
        columns: impl IntoIterator<Item = String>,
    ) {
        let cols = columns.into_iter().collect::<Vec<_>>();
        if let Some(existing) = self.tables.get(&relation)
            && existing.columns != cols
        {
            self.conflicted.insert(relation.clone());
        }
        self.tables.insert(
            relation.clone(),
            CatalogTable {
                name: preferred_name.into(),
                columns: cols.clone(),
                relation: relation.clone(),
            },
        );
        for view in &mut self.alias_views {
            if view.relation == relation {
                view.columns = cols.clone();
            }
        }
        self.aliases
            .entry(relation.clone())
            .or_default()
            .insert(relation);
    }

    /// Register a non-physical lookup alias for a physical relation.
    pub(crate) fn add_alias(&mut self, alias: RelationRef, relation: RelationRef) {
        debug_assert!(self.tables.contains_key(&relation));
        let inserted = self
            .aliases
            .entry(alias.clone())
            .or_default()
            .insert(relation.clone());
        if inserted {
            let physical = self
                .tables
                .get(&relation)
                .expect("catalog alias points at a registered relation");
            self.alias_views.push(CatalogTable {
                name: alias.render(),
                columns: physical.columns.clone(),
                relation,
            });
        }
    }

    pub(crate) fn contains_relation(&self, relation: &RelationRef) -> bool {
        self.tables.contains_key(relation)
    }

    pub(crate) fn resolve(&self, query: &RelationRef, dialect: DlinDialect) -> RelationResolution {
        self.resolve_filtered(query, dialect, false)
    }

    pub(crate) fn resolve_exact(
        &self,
        query: &RelationRef,
        dialect: DlinDialect,
    ) -> RelationResolution {
        self.resolve_filtered(query, dialect, true)
    }

    fn resolve_filtered(
        &self,
        query: &RelationRef,
        dialect: DlinDialect,
        exact_arity: bool,
    ) -> RelationResolution {
        let candidates = self
            .aliases
            .iter()
            .filter(|(alias, _)| {
                (!exact_arity || alias.qualification_len() == query.qualification_len())
                    && alias.matches(query, dialect)
            })
            .flat_map(|(_, relations)| relations.iter())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        match candidates.as_slice() {
            [] => RelationResolution::NotFound,
            [relation] => RelationResolution::Unique(
                self.tables
                    .keys()
                    .position(|candidate| candidate == relation)
                    .expect("catalog alias points at a registered relation"),
            ),
            _ => RelationResolution::Ambiguous,
        }
    }

    pub(crate) fn table_for_relation(&self, relation: &RelationRef) -> Option<&CatalogTable> {
        self.tables.get(relation)
    }

    pub(crate) fn resolve_table(
        &self,
        query: &RelationRef,
        dialect: DlinDialect,
    ) -> Option<&CatalogTable> {
        let RelationResolution::Unique(index) = self.resolve(query, dialect) else {
            return None;
        };
        self.tables.values().nth(index)
    }

    pub(crate) fn resolve_table_exact(
        &self,
        query: &RelationRef,
        dialect: DlinDialect,
    ) -> Option<&CatalogTable> {
        let RelationResolution::Unique(index) = self.resolve_exact(query, dialect) else {
            return None;
        };
        self.tables.values().nth(index)
    }

    pub(crate) fn unambiguous_columns_for_relation(
        &self,
        relation: &RelationRef,
    ) -> Option<&[String]> {
        if self.conflicted.contains(relation) {
            return None;
        }
        self.tables
            .get(relation)
            .map(|table| table.columns.as_slice())
    }

    pub fn table_columns(&self, name: &str) -> Option<&[String]> {
        let relation = RelationRef::parse(name).ok()?.as_manifest();
        let targets = self.aliases.get(&relation)?;
        if targets.len() != 1 {
            return None;
        }
        let target = targets.iter().next().expect("length checked above");
        self.tables
            .get(target)
            .map(|table| table.columns.as_slice())
    }

    pub(crate) fn unambiguous_table_columns(&self, name: &str) -> Option<&[String]> {
        let relation = RelationRef::parse(name).ok()?.as_manifest();
        let targets = self.aliases.get(&relation)?;
        if targets.len() != 1 {
            return None;
        }
        let target = targets.iter().next().expect("length checked above");
        if self.conflicted.contains(target) {
            return None;
        }
        self.tables
            .get(target)
            .map(|table| table.columns.as_slice())
    }

    pub fn tables(&self) -> impl Iterator<Item = &CatalogTable> {
        self.tables.values().chain(self.alias_views.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_snapshot_add_table_preserves_column_order() {
        let mut catalog = CatalogSnapshot::new();

        catalog.add_table("model_a", vec!["z".into(), "a".into(), "m".into()]);

        let cols = catalog
            .table_columns("model_a")
            .expect("table should exist")
            .to_vec();
        assert_eq!(
            cols,
            vec!["z".to_string(), "a".to_string(), "m".to_string()]
        );
    }

    #[test]
    fn catalog_snapshot_add_table_overwrites_with_latest_definition() {
        let mut catalog = CatalogSnapshot::new();

        catalog.add_table("model_a", vec!["a".into()]);
        catalog.add_table("model_a", vec!["b".into(), "a".into()]);

        let cols = catalog
            .table_columns("model_a")
            .expect("table should exist")
            .to_vec();
        assert_eq!(cols, vec!["b".to_string(), "a".to_string()]);
    }

    #[test]
    fn catalog_snapshot_identical_reregistration_stays_resolvable() {
        let mut catalog = CatalogSnapshot::new();
        catalog.add_table("model_a", vec!["a".into()]);
        catalog.add_table("model_a", vec!["a".into()]);

        assert_eq!(
            catalog.unambiguous_table_columns("model_a"),
            Some(["a".to_string()].as_slice())
        );
    }

    #[test]
    fn catalog_snapshot_differing_reregistration_becomes_unresolvable() {
        let mut catalog = CatalogSnapshot::new();
        catalog.add_table("model_a", vec!["a".into()]);
        catalog.add_table("model_a", vec!["b".into()]);

        assert_eq!(
            catalog.table_columns("model_a"),
            Some(["b".to_string()].as_slice())
        );
        assert_eq!(catalog.unambiguous_table_columns("model_a"), None);
    }

    #[test]
    fn catalog_snapshot_tables_iterates_in_order() {
        let mut catalog = CatalogSnapshot::new();
        catalog.add_table("c.model", vec!["id".into()]);
        catalog.add_table("a.model", vec!["id".into()]);
        catalog.add_table("b.model", vec!["id".into()]);

        let names: Vec<String> = catalog.tables().map(|table| table.name.clone()).collect();
        assert_eq!(
            names,
            vec![
                "a.model".to_string(),
                "b.model".to_string(),
                "c.model".to_string()
            ]
        );
    }

    #[test]
    fn structural_registration_keeps_dots_inside_aliases() {
        let mut catalog = CatalogSnapshot::new();
        let relation = RelationRef::from_manifest(Some("warehouse"), Some("raw"), "orders.v2");
        catalog.add_relation(relation.clone(), relation.render(), ["id".to_string()]);

        assert!(catalog.contains_relation(&relation));
        assert_eq!(catalog.tables().next().unwrap().relation, relation);
    }

    #[test]
    fn alias_collision_is_ambiguous_instead_of_last_write_wins() {
        let mut catalog = CatalogSnapshot::new();
        let first = RelationRef::from_manifest(Some("db_a"), Some("raw"), "orders");
        let second = RelationRef::from_manifest(Some("db_b"), Some("raw"), "orders");
        let alias = RelationRef::from_manifest(None, None, "orders");
        catalog.add_relation(first.clone(), first.render(), ["id".to_string()]);
        catalog.add_relation(second.clone(), second.render(), ["id".to_string()]);
        catalog.add_alias(alias.clone(), first);
        catalog.add_alias(alias.clone(), second);

        assert_eq!(catalog.table_columns("orders"), None);
    }
}
