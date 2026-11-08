use std::collections::{BTreeMap, BTreeSet};

use super::super::relation::{RelationRef, RelationResolution};
use super::super::schema::SchemaEvidence;
use super::dialect::DlinDialect;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogSnapshot {
    /// Physical relations are keyed by their structural identity. Lookup
    /// aliases are kept separately so an alias collision cannot overwrite a
    /// different physical relation. An alias is always resolved to its
    /// physical entry at lookup time and never holds a copy of the schema.
    tables: BTreeMap<RelationRef, CatalogTable>,
    aliases: BTreeMap<RelationRef, BTreeSet<RelationRef>>,
    conflicted: BTreeSet<RelationRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogTable {
    /// Preferred display spelling, normally derived from the manifest.
    pub name: String,
    pub(crate) relation: RelationRef,
    /// The typed schema: column spellings and ordinals, coverage, order,
    /// unknown segments and the evidence it rests on.
    pub(crate) schema: SchemaEvidence,
    /// Ids of every registration that makes the same claim as `schema`,
    /// including its own.
    pub(crate) evidence_ids: BTreeSet<String>,
}

impl CatalogTable {
    /// Column names in the order the schema lists them. Listing order is not
    /// evidence of physical order.
    pub fn column_names(&self) -> Vec<&str> {
        self.schema
            .columns
            .iter()
            .map(|column| column.name.value.as_str())
            .collect()
    }
}

/// A table or a lookup alias that resolves to one.
pub struct CatalogTableView<'a> {
    /// The spelling this entry is looked up by.
    pub name: String,
    table: &'a CatalogTable,
}

impl CatalogTableView<'_> {
    /// The physical relation this entry resolves to.
    #[cfg(test)]
    pub(crate) fn relation(&self) -> &RelationRef {
        &self.table.relation
    }

    pub fn column_names(&self) -> Vec<&str> {
        self.table.column_names()
    }
}

impl CatalogSnapshot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Compatibility entry point for callers that only have a relation
    /// string. New manifest code should use [`Self::add_relation`] and
    /// [`Self::add_alias`] so dots inside an identifier remain one component.
    ///
    /// A user-typed column list is an explicit, ordered enumeration, so it is
    /// registered with proven ordinals.
    pub fn add_table(
        &mut self,
        name: impl Into<String>,
        columns: impl IntoIterator<Item = String>,
    ) {
        let name = name.into();
        let relation = RelationRef::parse(&name)
            .map(|relation| relation.as_manifest())
            .unwrap_or_else(|_| RelationRef::bare(&name).as_manifest());
        let schema = SchemaEvidence::user_enumeration(&name, columns);
        self.add_relation(relation, name, schema);
    }

    /// Register one physical relation and its schema. Registering the same
    /// relation again with the same claim keeps the first schema and adds the
    /// new evidence id. A contradicting claim marks the relation as conflicted.
    pub(crate) fn add_relation(
        &mut self,
        relation: RelationRef,
        preferred_name: impl Into<String>,
        schema: SchemaEvidence,
    ) {
        if let Some(existing) = self.tables.get_mut(&relation)
            && existing.schema.same_claim_as(&schema)
        {
            existing.evidence_ids.insert(schema.id.0);
            return;
        }
        if self.tables.contains_key(&relation) {
            self.conflicted.insert(relation.clone());
        }
        self.tables.insert(
            relation.clone(),
            CatalogTable {
                name: preferred_name.into(),
                relation: relation.clone(),
                evidence_ids: BTreeSet::from([schema.id.0.clone()]),
                schema,
            },
        );
        self.aliases
            .entry(relation.clone())
            .or_default()
            .insert(relation);
    }

    /// Register a non-physical lookup alias for a physical relation.
    pub(crate) fn add_alias(&mut self, alias: RelationRef, relation: RelationRef) {
        debug_assert!(self.tables.contains_key(&relation));
        self.aliases.entry(alias).or_default().insert(relation);
    }

    pub(crate) fn contains_relation(&self, relation: &RelationRef) -> bool {
        self.tables.contains_key(relation)
    }

    /// Whether a relation was registered more than once with different schemas.
    pub(crate) fn is_conflicted(&self, relation: &RelationRef) -> bool {
        self.conflicted.contains(relation)
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

    /// Resolve a backend relation after its quote metadata and original case
    /// have been discarded. This fallback is deliberately opt-in at the
    /// catalog-provider boundary; the general relation matcher keeps its
    /// dialect-specific case policy.
    pub(crate) fn resolve_table_exact_case_insensitive(
        &self,
        query: &RelationRef,
    ) -> Option<&CatalogTable> {
        let candidates = self
            .aliases
            .iter()
            .filter(|(alias, _)| {
                alias.qualification_len() == query.qualification_len()
                    && alias.matches_case_insensitive(query)
            })
            .flat_map(|(_, relations)| relations.iter())
            .collect::<BTreeSet<_>>();
        let relation = (candidates.len() == 1)
            .then(|| candidates.iter().next().expect("length checked above"))?;
        self.tables.get(*relation)
    }

    pub fn table_columns(&self, name: &str) -> Option<Vec<&str>> {
        self.single_target_table(name)
            .map(CatalogTable::column_names)
    }

    #[cfg(test)]
    pub(crate) fn unambiguous_table_columns(&self, name: &str) -> Option<Vec<&str>> {
        self.single_target_table(name)
            .filter(|table| !self.is_conflicted(&table.relation))
            .map(CatalogTable::column_names)
    }

    fn single_target_table(&self, name: &str) -> Option<&CatalogTable> {
        let relation = RelationRef::parse(name).ok()?.as_manifest();
        let targets = self.aliases.get(&relation)?;
        if targets.len() != 1 {
            return None;
        }
        self.tables
            .get(targets.iter().next().expect("length checked above"))
    }

    fn has_single_target(&self, key: &RelationRef) -> bool {
        self.aliases
            .get(key)
            .is_some_and(|targets| targets.len() == 1)
    }

    /// Physical tables, followed by lookup aliases, that resolve to exactly
    /// one physical table. Aliases are resolved to the current physical entry.
    pub fn tables(&self) -> impl Iterator<Item = CatalogTableView<'_>> {
        let physical = self
            .tables
            .iter()
            .filter(|(relation, _)| self.has_single_target(relation))
            .map(|(_, table)| CatalogTableView {
                name: table.name.clone(),
                table,
            });
        let aliased = self
            .aliases
            .iter()
            .filter(|(alias, _)| self.has_single_target(alias))
            .flat_map(|(alias, targets)| {
                targets
                    .iter()
                    .filter(move |target| *target != alias)
                    .filter_map(|target| self.tables.get(target))
                    .map(move |table| CatalogTableView {
                        name: alias.render(),
                        table,
                    })
            });
        physical.chain(aliased)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::column_lineage::schema::evidence::{
        EvidenceId, EvidencedColumn, yaml_schema_evidence,
    };
    use crate::parser::manifest::ManifestColumn;

    fn schema(names: &[&str]) -> SchemaEvidence {
        SchemaEvidence::user_enumeration("t", names.iter().map(|name| (*name).to_string()))
    }

    fn yaml(names: &[(&str, Option<bool>)]) -> SchemaEvidence {
        let columns = names
            .iter()
            .map(|(name, quote)| {
                (
                    (*name).to_string(),
                    ManifestColumn {
                        name: (*name).to_string(),
                        quote: *quote,
                    },
                )
            })
            .collect();
        yaml_schema_evidence("source.p.s.t", &columns, None, DlinDialect::Generic).unwrap()
    }

    fn with_id(mut schema: SchemaEvidence, id: &str) -> SchemaEvidence {
        schema.id = EvidenceId(id.to_string());
        schema
    }

    fn register(catalog: &mut CatalogSnapshot, schema: SchemaEvidence) {
        let relation = RelationRef::from_manifest(None, None, "t");
        catalog.add_relation(relation.clone(), relation.render(), schema);
    }

    fn is_conflicted(catalog: &CatalogSnapshot) -> bool {
        catalog.is_conflicted(&RelationRef::from_manifest(None, None, "t"))
    }

    #[test]
    fn identical_typed_reregistration_is_not_a_conflict() {
        let mut catalog = CatalogSnapshot::new();
        register(&mut catalog, yaml(&[("a", None), ("b", Some(true))]));
        register(&mut catalog, yaml(&[("a", None), ("b", Some(true))]));
        assert!(!is_conflicted(&catalog));
    }

    #[test]
    fn same_names_with_different_quoting_conflict() {
        let mut catalog = CatalogSnapshot::new();
        register(&mut catalog, yaml(&[("a", None)]));
        register(&mut catalog, yaml(&[("a", Some(true))]));
        assert!(is_conflicted(&catalog));
    }

    #[test]
    fn unset_and_false_quote_specification_make_the_same_claim() {
        let mut catalog = CatalogSnapshot::new();
        register(&mut catalog, yaml(&[("a", None)]));
        register(&mut catalog, yaml(&[("a", Some(false))]));
        assert!(!is_conflicted(&catalog));
    }

    #[test]
    fn display_order_difference_under_unknown_order_is_not_a_conflict() {
        let mut catalog = CatalogSnapshot::new();
        register(&mut catalog, yaml(&[("a", None), ("b", None)]));
        register(&mut catalog, yaml(&[("b", None), ("a", None)]));
        assert!(!is_conflicted(&catalog));
        let table = catalog.tables().next().unwrap();
        assert_eq!(table.column_names(), vec!["a", "b"]);
    }

    #[test]
    fn different_evidence_ids_with_the_same_claim_coexist() {
        let mut catalog = CatalogSnapshot::new();
        register(&mut catalog, with_id(yaml(&[("a", None)]), "yaml:second"));
        register(&mut catalog, with_id(yaml(&[("a", None)]), "yaml:first"));
        register(&mut catalog, with_id(yaml(&[("a", None)]), "yaml:second"));
        assert!(!is_conflicted(&catalog));
        let table = catalog
            .resolve_table_exact(
                &RelationRef::from_manifest(None, None, "t"),
                DlinDialect::Generic,
            )
            .unwrap();
        assert_eq!(table.schema.id.0, "yaml:second");
        let ids: Vec<_> = table.evidence_ids.iter().map(String::as_str).collect();
        assert_eq!(ids, ["yaml:first", "yaml:second"]);
    }

    #[test]
    fn different_column_sets_conflict() {
        let mut catalog = CatalogSnapshot::new();
        register(&mut catalog, yaml(&[("a", None)]));
        register(&mut catalog, yaml(&[("a", None), ("b", None)]));
        assert!(is_conflicted(&catalog));
    }

    #[test]
    fn proven_order_difference_conflicts() {
        let mut catalog = CatalogSnapshot::new();
        register(&mut catalog, schema(&["a", "b"]));
        register(&mut catalog, schema(&["b", "a"]));
        assert!(is_conflicted(&catalog));
    }

    #[test]
    fn same_names_with_different_coverage_order_or_ordinals_conflict() {
        let base = yaml(&[("a", None)]);
        let variants: Vec<SchemaEvidence> = vec![
            SchemaEvidence {
                coverage: sqllineage::SchemaCoverage::Partial,
                ..base.clone()
            },
            SchemaEvidence {
                order: sqllineage::SchemaOrder::Proven,
                ..base.clone()
            },
            SchemaEvidence {
                columns: vec![EvidencedColumn {
                    ordinal: Some(0),
                    ..base.columns[0].clone()
                }],
                ..base.clone()
            },
            SchemaEvidence {
                unknown_segments: vec![sqllineage::CatalogUnknownSegment {
                    after_ordinal: None,
                }],
                ..base.clone()
            },
        ];
        for variant in variants {
            let mut catalog = CatalogSnapshot::new();
            register(&mut catalog, base.clone());
            register(&mut catalog, variant);
            assert!(is_conflicted(&catalog));
        }
    }

    #[test]
    fn duplicate_output_names_are_not_deduplicated() {
        let mut catalog = CatalogSnapshot::new();
        catalog.add_table("t", ["id".to_string(), "id".to_string()]);
        assert_eq!(catalog.table_columns("t"), Some(vec!["id", "id"]));
    }

    #[test]
    fn alias_lookup_sees_the_physical_entry_after_reregistration() {
        let mut catalog = CatalogSnapshot::new();
        let physical = RelationRef::from_manifest(Some("db"), Some("raw"), "orders");
        let alias = RelationRef::from_manifest(None, None, "orders");
        catalog.add_relation(physical.clone(), physical.render(), schema(&["a"]));
        catalog.add_alias(alias.clone(), physical.clone());
        catalog.add_relation(physical.clone(), physical.render(), schema(&["b", "c"]));

        let view = catalog
            .tables()
            .find(|table| table.name == alias.render())
            .expect("alias is listed");
        assert_eq!(view.relation(), &physical);
        assert_eq!(view.column_names(), vec!["b", "c"]);
        assert_eq!(catalog.table_columns("orders"), Some(vec!["b", "c"]));
    }

    #[test]
    fn catalog_snapshot_add_table_preserves_column_order() {
        let mut catalog = CatalogSnapshot::new();

        catalog.add_table("model_a", vec!["z".into(), "a".into(), "m".into()]);

        let cols = catalog
            .table_columns("model_a")
            .expect("table should exist");
        assert_eq!(cols, vec!["z", "a", "m"]);
    }

    #[test]
    fn catalog_snapshot_add_table_overwrites_with_latest_definition() {
        let mut catalog = CatalogSnapshot::new();

        catalog.add_table("model_a", vec!["a".into()]);
        catalog.add_table("model_a", vec!["b".into(), "a".into()]);

        let cols = catalog
            .table_columns("model_a")
            .expect("table should exist");
        assert_eq!(cols, vec!["b", "a"]);
    }

    #[test]
    fn catalog_snapshot_identical_reregistration_stays_resolvable() {
        let mut catalog = CatalogSnapshot::new();
        catalog.add_table("model_a", vec!["a".into()]);
        catalog.add_table("model_a", vec!["a".into()]);

        assert_eq!(
            catalog.unambiguous_table_columns("model_a"),
            Some(vec!["a"])
        );
    }

    #[test]
    fn catalog_snapshot_differing_reregistration_becomes_unresolvable() {
        let mut catalog = CatalogSnapshot::new();
        catalog.add_table("model_a", vec!["a".into()]);
        catalog.add_table("model_a", vec!["b".into()]);

        assert_eq!(catalog.table_columns("model_a"), Some(vec!["b"]));
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
        catalog.add_relation(relation.clone(), relation.render(), schema(&["id"]));

        assert!(catalog.contains_relation(&relation));
        assert_eq!(catalog.tables().next().unwrap().relation(), &relation);
    }

    #[test]
    fn alias_collision_is_ambiguous_instead_of_last_write_wins() {
        let mut catalog = CatalogSnapshot::new();
        let first = RelationRef::from_manifest(Some("db_a"), Some("raw"), "orders");
        let second = RelationRef::from_manifest(Some("db_b"), Some("raw"), "orders");
        let alias = RelationRef::from_manifest(None, None, "orders");
        catalog.add_relation(first.clone(), first.render(), schema(&["id"]));
        catalog.add_relation(second.clone(), second.render(), schema(&["id"]));
        catalog.add_alias(alias.clone(), first);
        catalog.add_alias(alias.clone(), second);

        assert_eq!(catalog.table_columns("orders"), None);
        let names: Vec<String> = catalog.tables().map(|table| table.name).collect();
        assert!(!names.iter().any(|name| name == "orders"));
    }

    #[test]
    fn ambiguous_physical_relation_is_hidden_from_tables() {
        let mut catalog = CatalogSnapshot::new();
        let bare = RelationRef::from_manifest(None, None, "orders");
        let qualified = RelationRef::from_manifest(Some("db_a"), Some("raw"), "orders");
        catalog.add_relation(bare.clone(), bare.render(), schema(&["id"]));
        catalog.add_relation(qualified.clone(), qualified.render(), schema(&["id"]));
        catalog.add_alias(bare, qualified.clone());

        let names: Vec<String> = catalog.tables().map(|table| table.name).collect();
        assert_eq!(names, vec![qualified.render()]);
        assert!(!names.iter().any(|name| name == "orders"));
    }
}
