use std::borrow::Cow;

use super::catalog::CatalogSnapshot;
use super::dialect::DlinDialect;
use crate::graph::column_lineage::relation::{Identifier, RelationRef};

pub(crate) struct CatalogAdapter<'a> {
    snapshot: &'a CatalogSnapshot,
    dialect: DlinDialect,
}

impl<'a> CatalogAdapter<'a> {
    pub(crate) const fn new(snapshot: &'a CatalogSnapshot, dialect: DlinDialect) -> Self {
        Self { snapshot, dialect }
    }
}

impl CatalogAdapter<'_> {
    /// Translate the stored schema into an engine entry without changing any
    /// of its claims. The entry owns its data.
    pub(crate) fn entry(
        &self,
        relation: &sqllineage::Relation,
    ) -> Option<sqllineage::CatalogEntry<'static>> {
        let query = relation_ref_from_engine(relation);
        let table = self
            .snapshot
            .resolve_table_exact(&query, self.dialect)
            .or_else(|| {
                allows_case_folding(&query, self.dialect)
                    .then(|| self.snapshot.resolve_table_exact_case_insensitive(&query))
                    .flatten()
            })?;
        if self.snapshot.is_conflicted(&table.relation) {
            return None;
        }
        let schema = &table.schema;
        Some(sqllineage::CatalogEntry {
            relation: relation_from_ref(&table.relation),
            columns: Cow::Owned(
                schema
                    .columns
                    .iter()
                    .map(|column| sqllineage::CatalogColumn {
                        name: column.name.clone(),
                        ordinal: column.ordinal,
                        data_type: None,
                        fields: None,
                    })
                    .collect(),
            ),
            coverage: schema.coverage,
            order: schema.order,
            unknown_segments: Cow::Owned(schema.unknown_segments.clone()),
            evidence: Cow::Owned(
                table
                    .evidence_ids
                    .iter()
                    .cloned()
                    .map(sqllineage::EvidenceRef)
                    .collect(),
            ),
        })
    }
}

impl sqllineage::Catalog for CatalogAdapter<'_> {
    fn lookup(&self, relation: &sqllineage::Relation) -> Option<sqllineage::CatalogEntry<'_>> {
        self.entry(relation)
    }
}

fn relation_ref_from_engine(relation: &sqllineage::Relation) -> RelationRef {
    use crate::graph::column_lineage::relation::QuoteStyle;
    use sqllineage::{Ident, Quote};

    let component = |ident: &Ident| match ident.quote {
        Some(Quote::Double | Quote::Bracket | Quote::Single) => {
            Identifier::quoted(ident.value.clone(), QuoteStyle::Double)
        }
        Some(Quote::Backtick) => Identifier::quoted(ident.value.clone(), QuoteStyle::Backtick),
        None => Identifier::unquoted(ident.value.clone()),
    };
    RelationRef::from_identifiers(
        relation.catalog.as_ref().map(component),
        relation.schema.as_ref().map(component),
        component(&relation.name),
    )
}

/// Keep dlin's source spelling stable while the engine retains SQL quote metadata.
pub(crate) fn source_relation(
    relation: &sqllineage::Relation,
    catalog: Option<&CatalogSnapshot>,
    dialect: DlinDialect,
) -> RelationRef {
    let query = relation_ref_from_engine(relation);
    if let Some(table) = catalog.and_then(|catalog| {
        catalog.resolve_table_exact(&query, dialect).or_else(|| {
            allows_case_folding(&query, dialect)
                .then(|| catalog.resolve_table_exact_case_insensitive(&query))
                .flatten()
        })
    }) {
        return table.relation.clone();
    }
    let part = |ident: &sqllineage::Ident| {
        if ident.quote.is_some() {
            ident.value.clone()
        } else {
            ident.value.to_lowercase()
        }
    };
    let catalog = relation.catalog.as_ref().map(part);
    let schema = relation.schema.as_ref().map(part);
    let name = part(&relation.name);
    RelationRef::from_backend(catalog.as_deref(), schema.as_deref(), &name)
}

fn relation_from_ref(relation: &RelationRef) -> sqllineage::Relation {
    use sqllineage::{Ident, Quote};
    let ident = |part: &Identifier| match part.quote_style() {
        crate::graph::column_lineage::relation::QuoteStyle::Double => {
            Ident::quoted(part.value().to_owned(), Quote::Double)
        }
        crate::graph::column_lineage::relation::QuoteStyle::Backtick => {
            Ident::quoted(part.value().to_owned(), Quote::Backtick)
        }
        crate::graph::column_lineage::relation::QuoteStyle::Unquoted
        | crate::graph::column_lineage::relation::QuoteStyle::Unknown => {
            Ident::unquoted(part.value().to_owned())
        }
    };
    sqllineage::Relation {
        catalog: relation.catalog().map(ident),
        schema: relation.schema().map(ident),
        name: ident(relation.name()),
    }
}

fn allows_case_folding(query: &RelationRef, dialect: DlinDialect) -> bool {
    use crate::graph::column_lineage::relation::QuoteStyle;
    matches!(dialect, DlinDialect::Generic | DlinDialect::BigQuery)
        && query
            .catalog()
            .into_iter()
            .chain(query.schema())
            .chain(std::iter::once(query.name()))
            .all(|part| part.quote_style() == QuoteStyle::Unquoted)
}

#[cfg(test)]
mod source_relation_tests {
    use super::*;

    #[test]
    fn quoted_sources_do_not_use_case_insensitive_catalog_fallback() {
        let mut catalog = CatalogSnapshot::new();
        catalog.add_table("Users", ["id".to_string()]);
        for dialect in [DlinDialect::Generic, DlinDialect::BigQuery] {
            let mut relation = sqllineage::Relation {
                catalog: None,
                schema: None,
                name: sqllineage::Ident::quoted("users", sqllineage::Quote::Double),
            };
            assert_eq!(
                source_relation(&relation, Some(&catalog), dialect)
                    .name()
                    .value(),
                "users"
            );
            relation.name = sqllineage::Ident::unquoted("users");
            assert_eq!(
                source_relation(&relation, Some(&catalog), dialect)
                    .name()
                    .value(),
                "Users"
            );
        }
    }
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct SqllineageCatalogProvider {
    snapshot: CatalogSnapshot,
    dialect: DlinDialect,
}

#[cfg(test)]
impl SqllineageCatalogProvider {
    pub(crate) fn new(snapshot: &CatalogSnapshot, dialect: DlinDialect) -> Self {
        Self {
            snapshot: snapshot.clone(),
            dialect,
        }
    }
}

#[cfg(test)]
impl sqllineage::Catalog for SqllineageCatalogProvider {
    fn lookup(&self, relation: &sqllineage::Relation) -> Option<sqllineage::CatalogEntry<'_>> {
        let adapter = CatalogAdapter::new(&self.snapshot, self.dialect);
        adapter.entry(relation)
    }
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TableRef {
    pub catalog: Option<String>,
    pub schema: Option<String>,
    pub table: String,
}

#[cfg(test)]
impl SqllineageCatalogProvider {
    pub(crate) fn list_columns(&self, table: &TableRef) -> Option<Vec<String>> {
        let query = RelationRef::from_backend_lookup(
            table.catalog.as_deref(),
            table.schema.as_deref(),
            &table.table,
        );
        let found = match self.dialect {
            DlinDialect::Generic | DlinDialect::BigQuery => {
                self.snapshot.resolve_table_exact_case_insensitive(&query)
            }
            _ => self.snapshot.resolve_table_exact(&query, self.dialect),
        }?;
        (!self.snapshot.is_conflicted(&found.relation)).then(|| {
            found
                .column_names()
                .into_iter()
                .map(str::to_owned)
                .collect()
        })
    }

    pub(crate) fn resolve_column(&self, column: &str, candidates: &[TableRef]) -> Option<TableRef> {
        let matching = candidates
            .iter()
            .filter(|candidate| {
                self.list_columns(candidate).is_some_and(|columns| {
                    columns
                        .iter()
                        .any(|known| known.to_lowercase() == column.to_lowercase())
                })
            })
            .collect::<Vec<_>>();
        (matching.len() == 1).then(|| (*matching[0]).clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(catalog: Option<&str>, schema: Option<&str>, table: &str) -> TableRef {
        TableRef {
            catalog: catalog.map(str::to_string),
            schema: schema.map(str::to_string),
            table: table.to_string(),
        }
    }

    fn catalog() -> CatalogSnapshot {
        let mut snapshot = CatalogSnapshot::new();
        snapshot.add_table("orders", ["id".to_string()]);
        snapshot.add_table("analytics.orders", ["id".to_string(), "total".to_string()]);
        snapshot.add_table(
            "warehouse.analytics.orders",
            ["id".to_string(), "total".to_string(), "region".to_string()],
        );
        snapshot
    }

    fn relation(name: &str) -> sqllineage::Relation {
        sqllineage::Relation {
            catalog: None,
            schema: None,
            name: sqllineage::Ident::unquoted(name),
        }
    }

    fn yaml_columns(
        columns: &[(&str, Option<bool>)],
    ) -> indexmap::IndexMap<String, crate::parser::manifest::ManifestColumn> {
        columns
            .iter()
            .map(|(name, quote)| {
                (
                    (*name).to_string(),
                    crate::parser::manifest::ManifestColumn {
                        name: (*name).to_string(),
                        quote: *quote,
                    },
                )
            })
            .collect()
    }

    fn register_yaml(
        snapshot: &mut CatalogSnapshot,
        name: &str,
        columns: &[(&str, Option<bool>)],
        dialect: DlinDialect,
    ) {
        register_yaml_evidence(
            snapshot,
            &format!("source.proj.raw.{name}"),
            name,
            columns,
            dialect,
        );
    }

    /// Register the relation `src` from the YAML declaration of `subject`.
    fn register_yaml_as(
        snapshot: &mut CatalogSnapshot,
        subject: &str,
        columns: &[(&str, Option<bool>)],
    ) {
        register_yaml_evidence(snapshot, subject, "src", columns, DlinDialect::Generic);
    }

    fn register_yaml_evidence(
        snapshot: &mut CatalogSnapshot,
        subject: &str,
        name: &str,
        columns: &[(&str, Option<bool>)],
        dialect: DlinDialect,
    ) {
        let evidence = crate::graph::column_lineage::schema::evidence::yaml_schema_evidence(
            subject,
            &yaml_columns(columns),
            None,
            dialect,
        )
        .expect("non-empty column list");
        let relation = RelationRef::from_manifest(None, None, name);
        snapshot.add_relation(relation.clone(), relation.render(), evidence);
    }

    #[test]
    fn yaml_only_table_is_exhaustive_with_unknown_order_and_evidence() {
        let mut snapshot = CatalogSnapshot::new();
        register_yaml(
            &mut snapshot,
            "src",
            &[("z", None), ("a", None)],
            DlinDialect::Generic,
        );
        let adapter = CatalogAdapter::new(&snapshot, DlinDialect::Generic);

        let entry = sqllineage::Catalog::lookup(&adapter, &relation("src")).unwrap();

        assert_eq!(entry.coverage, sqllineage::SchemaCoverage::Exhaustive);
        assert_eq!(entry.order, sqllineage::SchemaOrder::Unknown);
        let names: Vec<_> = entry
            .columns
            .iter()
            .map(|c| c.name.value.as_str())
            .collect();
        assert_eq!(names, ["z", "a"]);
        assert!(entry.columns.iter().all(|column| column.ordinal.is_none()));
        assert!(entry.unknown_segments.is_empty());
        assert_eq!(
            entry.evidence.as_ref(),
            [sqllineage::EvidenceRef(
                "yaml:source.proj.raw.src".to_string()
            )]
        );
    }

    #[test]
    fn consistent_registrations_expose_every_evidence_id() {
        let mut snapshot = CatalogSnapshot::new();
        register_yaml_as(
            &mut snapshot,
            "source.proj.a.src",
            &[("z", None), ("a", None)],
        );
        register_yaml_as(
            &mut snapshot,
            "source.proj.b.src",
            &[("z", None), ("a", None)],
        );
        let adapter = CatalogAdapter::new(&snapshot, DlinDialect::Generic);

        let entry = sqllineage::Catalog::lookup(&adapter, &relation("src")).unwrap();

        assert_eq!(
            entry.evidence.as_ref(),
            [
                sqllineage::EvidenceRef("yaml:source.proj.a.src".to_string()),
                sqllineage::EvidenceRef("yaml:source.proj.b.src".to_string()),
            ]
        );
    }

    #[test]
    fn reordered_consistent_registration_keeps_the_first_display_order() {
        let mut snapshot = CatalogSnapshot::new();
        register_yaml_as(
            &mut snapshot,
            "source.proj.a.src",
            &[("z", None), ("a", None)],
        );
        register_yaml_as(
            &mut snapshot,
            "source.proj.b.src",
            &[("a", None), ("z", None)],
        );
        let adapter = CatalogAdapter::new(&snapshot, DlinDialect::Generic);

        let entry = sqllineage::Catalog::lookup(&adapter, &relation("src")).unwrap();

        let names: Vec<_> = entry
            .columns
            .iter()
            .map(|c| c.name.value.as_str())
            .collect();
        assert_eq!(names, ["z", "a"]);
        assert_eq!(entry.evidence.len(), 2);
    }

    #[test]
    fn contradicting_registration_has_no_entry() {
        let mut snapshot = CatalogSnapshot::new();
        register_yaml_as(&mut snapshot, "source.proj.a.src", &[("z", None)]);
        register_yaml_as(
            &mut snapshot,
            "source.proj.b.src",
            &[("z", None), ("a", None)],
        );
        let adapter = CatalogAdapter::new(&snapshot, DlinDialect::Generic);

        assert!(sqllineage::Catalog::lookup(&adapter, &relation("src")).is_none());
    }

    #[test]
    fn quote_true_reaches_the_engine_as_a_quoted_identifier() {
        for (dialect, quote) in [
            (DlinDialect::Snowflake, sqllineage::Quote::Double),
            (DlinDialect::BigQuery, sqllineage::Quote::Backtick),
        ] {
            let mut snapshot = CatalogSnapshot::new();
            register_yaml(
                &mut snapshot,
                "src",
                &[("id", Some(true)), ("Name", Some(false)), ("plain", None)],
                dialect,
            );
            let adapter = CatalogAdapter::new(&snapshot, dialect);

            let entry = sqllineage::Catalog::lookup(&adapter, &relation("src")).unwrap();

            let names: Vec<_> = entry.columns.iter().map(|c| c.name.clone()).collect();
            assert_eq!(
                names,
                [
                    sqllineage::Ident::quoted("id", quote),
                    sqllineage::Ident::unquoted("Name"),
                    sqllineage::Ident::unquoted("plain"),
                ]
            );
        }
    }

    #[test]
    fn empty_exhaustive_table_is_an_entry_with_no_columns() {
        let mut snapshot = CatalogSnapshot::new();
        snapshot.add_table("empty", Vec::<String>::new());
        let adapter = CatalogAdapter::new(&snapshot, DlinDialect::Generic);

        let entry = sqllineage::Catalog::lookup(&adapter, &relation("empty")).unwrap();

        assert!(entry.columns.is_empty());
        assert_eq!(entry.coverage, sqllineage::SchemaCoverage::Exhaustive);
        assert!(sqllineage::Catalog::lookup(&adapter, &relation("missing")).is_none());
    }

    #[test]
    fn user_typed_schema_has_proven_order_and_ordinals() {
        let mut snapshot = CatalogSnapshot::new();
        snapshot.add_table("t", ["b".to_string(), "a".to_string(), "b".to_string()]);
        let adapter = CatalogAdapter::new(&snapshot, DlinDialect::Generic);

        let entry = sqllineage::Catalog::lookup(&adapter, &relation("t")).unwrap();

        assert_eq!(entry.coverage, sqllineage::SchemaCoverage::Exhaustive);
        assert_eq!(entry.order, sqllineage::SchemaOrder::Proven);
        let columns: Vec<_> = entry
            .columns
            .iter()
            .map(|c| (c.name.value.as_str(), c.ordinal))
            .collect();
        assert_eq!(columns, [("b", Some(0)), ("a", Some(1)), ("b", Some(2))]);
    }

    #[test]
    fn conflicted_table_has_no_entry() {
        let mut snapshot = CatalogSnapshot::new();
        snapshot.add_table("t", ["a".to_string()]);
        snapshot.add_table("t", ["b".to_string()]);
        let adapter = CatalogAdapter::new(&snapshot, DlinDialect::Generic);

        assert!(sqllineage::Catalog::lookup(&adapter, &relation("t")).is_none());
    }

    #[test]
    fn list_columns_requires_exact_relation_arity() {
        let snapshot = catalog();
        let provider = SqllineageCatalogProvider::new(&snapshot, DlinDialect::Generic);

        assert_eq!(
            provider
                .list_columns(&table(None, None, "orders"))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            provider
                .list_columns(&table(None, Some("analytics"), "orders"))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            provider
                .list_columns(&table(Some("warehouse"), Some("analytics"), "orders"))
                .unwrap()
                .len(),
            3
        );
        assert!(
            provider
                .list_columns(&table(None, Some("warehouse"), "analytics.orders"))
                .is_none()
        );
        assert!(
            provider
                .list_columns(&table(None, Some("analytics"), "missing"))
                .is_none()
        );
    }

    #[test]
    fn list_columns_folds_case_beyond_ascii() {
        let mut snapshot = CatalogSnapshot::new();
        // sqllineage lowercases an unquoted identifier with `to_lowercase`, so a
        // cased non-ASCII relation reaches the provider already folded.
        snapshot.add_table("Заказы", ["id".to_string()]);
        let provider = SqllineageCatalogProvider::new(&snapshot, DlinDialect::Snowflake);

        assert!(
            provider
                .list_columns(&table(None, None, "заказы"))
                .is_some()
        );
    }

    #[test]
    fn list_columns_matches_bigquery_identifiers_case_insensitively() {
        let mut snapshot = CatalogSnapshot::new();
        snapshot.add_table("Warehouse.Analytics.Orders", ["id".to_string()]);
        let provider = SqllineageCatalogProvider::new(&snapshot, DlinDialect::BigQuery);

        assert!(
            provider
                .list_columns(&table(Some("WAREHOUSE"), Some("ANALYTICS"), "ORDERS"))
                .is_some()
        );
    }

    #[test]
    fn list_columns_matches_mixed_case_generic_relation_after_backend_lowercasing() {
        let mut snapshot = CatalogSnapshot::new();
        snapshot.add_table("Warehouse.Analytics.Orders", ["id".to_string()]);
        let provider = SqllineageCatalogProvider::new(&snapshot, DlinDialect::Generic);

        assert!(
            provider
                .list_columns(&table(Some("warehouse"), Some("analytics"), "orders"))
                .is_some()
        );
    }

    #[test]
    fn list_columns_keeps_empty_tables_and_rejects_conflicted_ones() {
        let mut snapshot = CatalogSnapshot::new();
        snapshot.add_table("empty", Vec::<String>::new());
        snapshot.add_table("conflicted", ["id".to_string()]);
        snapshot.add_table("conflicted", ["other".to_string()]);
        let provider = SqllineageCatalogProvider::new(&snapshot, DlinDialect::Generic);

        assert_eq!(
            provider.list_columns(&table(None, None, "empty")),
            Some(Vec::new())
        );
        assert!(
            provider
                .list_columns(&table(None, None, "conflicted"))
                .is_none()
        );
    }

    #[test]
    fn resolve_column_requires_exactly_one_qualifying_candidate() {
        let snapshot = catalog();
        let provider = SqllineageCatalogProvider::new(&snapshot, DlinDialect::Generic);
        let one = table(None, None, "orders");
        let two = table(None, Some("analytics"), "orders");
        let unknown = table(None, None, "missing");

        assert_eq!(
            provider.resolve_column("id", std::slice::from_ref(&one)),
            Some(one.clone())
        );
        assert!(
            provider
                .resolve_column("id", &[one.clone(), two.clone()])
                .is_none()
        );
        assert!(provider.resolve_column("id", &[unknown]).is_none());
        assert!(provider.resolve_column("missing", &[one]).is_none());
    }

    #[test]
    fn resolve_column_folds_catalog_column_case() {
        let mut snapshot = CatalogSnapshot::new();
        snapshot.add_table("orders", ["ID".to_string()]);
        let provider = SqllineageCatalogProvider::new(&snapshot, DlinDialect::Generic);

        assert_eq!(
            provider.resolve_column("id", &[table(None, None, "orders")]),
            Some(table(None, None, "orders"))
        );
    }
}
