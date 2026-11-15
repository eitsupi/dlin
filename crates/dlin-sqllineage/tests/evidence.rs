//! Schema evidence attached to output columns: each conclusion lists the
//! catalog entries it relies on, including the schemas that ruled other
//! candidates out, and no entry that did not decide anything.

use std::borrow::Cow;

use dlin_sqllineage::{
    Analyzer, Catalog, CatalogColumn, CatalogEntry, ColumnEvidence, Dialect, EvidenceRef, Ident,
    Lineage, OutputColumn, Relation, SchemaCoverage, SchemaOrder, Shape, SourceColumn,
    StatementAnalysis, Unresolved,
};

fn relation(name: &str) -> Relation {
    Relation {
        catalog: None,
        schema: None,
        name: Ident::unquoted(name),
    }
}

fn source(table: &str, column: &str) -> SourceColumn {
    SourceColumn {
        relation: relation(table),
        column: Ident::unquoted(column),
    }
}

/// The evidence the test catalog gives the entry of `table`.
fn ev(table: &str) -> EvidenceRef {
    EvidenceRef(format!("schema:{table}"))
}

fn evs(tables: &[&str]) -> Vec<EvidenceRef> {
    tables.iter().map(|table| ev(table)).collect()
}

struct Entry {
    name: &'static str,
    columns: Vec<CatalogColumn>,
    coverage: SchemaCoverage,
    order: SchemaOrder,
}

/// A catalog whose entry for each table carries the evidence `schema:<table>`.
struct Schemas(Vec<Entry>);

impl Schemas {
    fn new() -> Self {
        Self(Vec::new())
    }

    fn table(
        mut self,
        name: &'static str,
        columns: &[&str],
        coverage: SchemaCoverage,
        order: SchemaOrder,
    ) -> Self {
        self.0.push(Entry {
            name,
            columns: columns
                .iter()
                .enumerate()
                .map(|(ordinal, column)| CatalogColumn {
                    name: Ident::unquoted(*column),
                    ordinal: (order == SchemaOrder::Proven).then_some(ordinal),
                    data_type: None,
                    fields: None,
                })
                .collect(),
            coverage,
            order,
        });
        self
    }

    /// A table whose listed columns are all of them, in proven order.
    fn exhaustive(self, name: &'static str, columns: &[&str]) -> Self {
        self.table(
            name,
            columns,
            SchemaCoverage::Exhaustive,
            SchemaOrder::Proven,
        )
    }

    /// A table that may have columns beyond those listed.
    fn partial(self, name: &'static str, columns: &[&str]) -> Self {
        self.table(name, columns, SchemaCoverage::Partial, SchemaOrder::Unknown)
    }
}

impl Catalog for Schemas {
    fn lookup(&self, query: &Relation) -> Option<CatalogEntry<'_>> {
        self.0
            .iter()
            .find(|entry| relation(entry.name) == *query)
            .map(|entry| CatalogEntry {
                relation: relation(entry.name),
                columns: Cow::Borrowed(&entry.columns),
                coverage: entry.coverage,
                order: entry.order,
                unknown_segments: Cow::Borrowed(&[]),
                evidence: Cow::Owned(vec![ev(entry.name)]),
            })
    }
}

fn analyze(sql: &str, catalog: &Schemas) -> StatementAnalysis {
    Analyzer::new(Dialect::Generic)
        .with_catalog(catalog)
        .analyze(sql)
        .unwrap_or_else(|error| panic!("failed to analyze {sql}: {error}"))
        .into_single()
        .expect("one statement")
}

fn column<'a>(result: &'a StatementAnalysis, name: &str) -> &'a OutputColumn {
    match result.output.lookup(name, Dialect::Generic) {
        dlin_sqllineage::Lookup::Found(column) => column,
        other => panic!("expected one column {name}, got {other:?}"),
    }
}

fn known_evidence(evidence: &ColumnEvidence, wanted: &SourceColumn) -> Vec<EvidenceRef> {
    evidence
        .known
        .iter()
        .find(|known| known.source == *wanted)
        .map(|known| known.evidence.clone())
        .unwrap_or_default()
}

#[test]
fn a_name_bound_by_ruling_out_another_relation_relies_on_both_schemas() {
    let catalog = Schemas::new()
        .exhaustive("a", &["x"])
        .exhaustive("b", &["y"]);
    let result = analyze("SELECT x FROM a CROSS JOIN b", &catalog);
    let x = column(&result, "x");
    assert_eq!(x.lineage, Lineage::Complete(vec![source("a", "x")]));
    // b's schema proves it has no x, which is what makes a.x the column read.
    assert_eq!(
        known_evidence(&x.evidence, &source("a", "x")),
        evs(&["a", "b"])
    );
    assert_eq!(x.evidence.lineage, evs(&["a", "b"]));
}

#[test]
fn a_partial_schema_leaves_an_ambiguity_that_relies_on_both_schemas() {
    let catalog = Schemas::new().exhaustive("a", &["x"]).partial("b", &["y"]);
    let result = analyze("SELECT x FROM a CROSS JOIN b", &catalog);
    let x = column(&result, "x");
    let Lineage::Partial { unresolved, .. } = &x.lineage else {
        panic!("b may hide x: {:?}", x.lineage);
    };
    assert!(matches!(
        unresolved.as_slice(),
        [Unresolved::Ambiguous { .. }]
    ));
    assert_eq!(x.evidence.unresolved.len(), 1);
    assert_eq!(x.evidence.unresolved[0].evidence, evs(&["a", "b"]));
    assert_eq!(x.evidence.lineage, evs(&["a", "b"]));
}

#[test]
fn an_empty_exhaustive_schema_still_rules_a_name_out() {
    let catalog = Schemas::new().exhaustive("a", &["x"]).exhaustive("b", &[]);
    let result = analyze("SELECT x FROM a CROSS JOIN b", &catalog);
    let x = column(&result, "x");
    assert_eq!(x.lineage, Lineage::Complete(vec![source("a", "x")]));
    assert_eq!(x.evidence.lineage, evs(&["a", "b"]));
}

#[test]
fn a_qualified_name_relies_only_on_its_own_relation() {
    let catalog = Schemas::new()
        .exhaustive("a", &["x"])
        .exhaustive("b", &["y"]);
    let result = analyze("SELECT a.x FROM a CROSS JOIN b", &catalog);
    let x = column(&result, "x");
    assert_eq!(x.evidence.lineage, evs(&["a"]));
    assert_eq!(x.evidence.structure, Vec::<EvidenceRef>::new());
    assert_eq!(result.output.evidence, Vec::<EvidenceRef>::new());
}

#[test]
fn a_source_free_value_relies_on_no_schema() {
    let catalog = Schemas::new().exhaustive("a", &["x"]);
    let result = analyze("SELECT 1 AS one, count(*) AS n FROM a", &catalog);
    for name in ["one", "n"] {
        assert_eq!(column(&result, name).evidence, ColumnEvidence::default());
    }
}

#[test]
fn a_source_free_column_chosen_over_another_relation_keeps_that_relations_evidence() {
    let catalog = Schemas::new().exhaustive("b", &["y"]);
    let result = analyze(
        "WITH c AS (SELECT 1 AS x) SELECT x FROM c CROSS JOIN b",
        &catalog,
    );
    let x = column(&result, "x");
    assert_eq!(x.lineage, Lineage::Complete(Vec::new()));
    assert_eq!(x.evidence.lineage, evs(&["b"]));
}

#[test]
fn evidence_follows_a_column_through_ctes_and_star_wrappers() {
    let catalog = Schemas::new()
        .exhaustive("a", &["x", "z"])
        .exhaustive("b", &["y"]);
    let result = analyze(
        "WITH c AS (SELECT * FROM a), \
         d AS (SELECT * RENAME (x AS w) FROM c) \
         SELECT w, y FROM (SELECT * FROM d) e CROSS JOIN b",
        &catalog,
    );
    let w = column(&result, "w");
    assert_eq!(w.lineage, Lineage::Complete(vec![source("a", "x")]));
    assert_eq!(
        known_evidence(&w.evidence, &source("a", "x")),
        evs(&["a", "b"])
    );
    let y = column(&result, "y");
    assert_eq!(
        known_evidence(&y.evidence, &source("b", "y")),
        evs(&["a", "b"])
    );
}

#[test]
fn separate_outputs_and_separate_sources_keep_their_own_evidence() {
    let catalog = Schemas::new()
        .exhaustive("a", &["x"])
        .exhaustive("b", &["y"]);
    let result = analyze(
        "SELECT a.x, b.y, a.x + b.y AS total FROM a CROSS JOIN b",
        &catalog,
    );
    assert_eq!(column(&result, "x").evidence.lineage, evs(&["a"]));
    assert_eq!(column(&result, "y").evidence.lineage, evs(&["b"]));
    let total = column(&result, "total");
    assert_eq!(total.evidence.lineage, evs(&["a", "b"]));
    assert_eq!(
        known_evidence(&total.evidence, &source("a", "x")),
        evs(&["a"])
    );
    assert_eq!(
        known_evidence(&total.evidence, &source("b", "y")),
        evs(&["b"])
    );
}

#[test]
fn star_columns_rely_on_the_schemas_that_place_them() {
    let catalog = Schemas::new()
        .exhaustive("a", &["x"])
        .exhaustive("b", &["y"]);
    let result = analyze("SELECT a.*, b.*, 1 AS k FROM a CROSS JOIN b", &catalog);
    assert_eq!(result.output.shape, Shape::Closed);
    assert_eq!(column(&result, "x").evidence.structure, evs(&["a"]));
    // y's position follows the width of a.
    assert_eq!(column(&result, "y").evidence.structure, evs(&["a", "b"]));
    assert_eq!(column(&result, "k").evidence.structure, evs(&["a", "b"]));
    assert_eq!(result.output.evidence, evs(&["a", "b"]));
}

#[test]
fn positional_correspondence_relies_on_both_operands_including_except() {
    let catalog = Schemas::new()
        .exhaustive("a", &["x"])
        .exhaustive("b", &["y"]);
    for operator in ["UNION ALL", "EXCEPT"] {
        let result = analyze(
            &format!("SELECT * FROM a {operator} SELECT * FROM b"),
            &catalog,
        );
        let x = column(&result, "x");
        assert_eq!(x.evidence.lineage, evs(&["a", "b"]), "{operator}");
        assert_eq!(
            known_evidence(&x.evidence, &source("a", "x")),
            evs(&["a", "b"]),
            "{operator}"
        );
    }
    let union = analyze("SELECT * FROM a UNION ALL SELECT * FROM b", &catalog);
    assert_eq!(
        known_evidence(&column(&union, "x").evidence, &source("b", "y")),
        evs(&["a", "b"])
    );
    let except = analyze("SELECT * FROM a EXCEPT SELECT * FROM b", &catalog);
    assert_eq!(
        column(&except, "x").lineage,
        Lineage::Complete(vec![source("a", "x")])
    );
}

#[test]
fn an_unknown_order_leaves_an_alignment_that_relies_on_both_operands() {
    let catalog = Schemas::new().exhaustive("a", &["x", "z"]).table(
        "b",
        &["y", "w"],
        SchemaCoverage::Exhaustive,
        SchemaOrder::Unknown,
    );
    let result = analyze("SELECT * FROM a UNION ALL SELECT * FROM b", &catalog);
    let x = column(&result, "x");
    let Lineage::Partial { unresolved, .. } = &x.lineage else {
        panic!("b's order is unknown: {:?}", x.lineage);
    };
    assert!(matches!(
        unresolved.as_slice(),
        [Unresolved::AlignmentUnknown { .. }]
    ));
    assert_eq!(x.evidence.unresolved[0].evidence, evs(&["a", "b"]));
}

#[test]
fn a_source_reached_along_several_paths_lists_its_evidence_once() {
    let catalog = Schemas::new()
        .exhaustive("a", &["x"])
        .exhaustive("b", &["y"]);
    let result = analyze("SELECT a.x + x AS twice FROM a CROSS JOIN b", &catalog);
    let twice = column(&result, "twice");
    assert_eq!(twice.lineage, Lineage::Complete(vec![source("a", "x")]));
    assert_eq!(twice.evidence.known.len(), 1);
    assert_eq!(
        known_evidence(&twice.evidence, &source("a", "x")),
        evs(&["a", "b"])
    );
}

#[test]
fn without_a_catalog_nothing_relies_on_evidence() {
    let result = Analyzer::new(Dialect::Generic)
        .analyze("SELECT x, a.* FROM a CROSS JOIN b")
        .unwrap()
        .into_single()
        .unwrap();
    assert!(result.output.evidence.is_empty());
    for column in &result.output.columns {
        assert_eq!(column.evidence, ColumnEvidence::default());
    }
}
