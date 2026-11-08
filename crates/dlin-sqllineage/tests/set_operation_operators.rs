//! Which operand of a set operation supplies values to the output. `UNION`
//! and `INTERSECT` take values from both operands, `EXCEPT` from the left
//! operand only. Uncertainty on the right of an `EXCEPT` never reaches the
//! value lineage.

use std::borrow::Cow;

use dlin_sqllineage::{
    Analysis, Analyzer, Catalog, CatalogColumn, CatalogEntry, DiagnosticKind, Dialect, EvidenceRef,
    Ident, Lineage, Relation, RowDependencies, SchemaCoverage, SchemaOrder, SetOperator, Shape,
    SourceColumn, StatementAnalysis, Unresolved,
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

fn analyze_in(dialect: Dialect, sql: &str) -> StatementAnalysis {
    Analyzer::new(dialect)
        .analyze(sql)
        .unwrap_or_else(|error| panic!("failed to analyze {sql}: {error}"))
        .into_single()
        .expect("one statement")
}

fn analyze(sql: &str) -> StatementAnalysis {
    analyze_in(Dialect::Generic, sql)
}

fn assert_complete(statement: &StatementAnalysis, column: usize, expected: &[SourceColumn]) {
    match &statement.output.columns[column].lineage {
        Lineage::Complete(sources) => assert_eq!(sources, expected),
        other => panic!("expected complete lineage, got {other:?}"),
    }
}

fn partial(statement: &StatementAnalysis, column: usize) -> (&[SourceColumn], &[Unresolved]) {
    match &statement.output.columns[column].lineage {
        Lineage::Partial { known, unresolved } => (known, unresolved),
        other => panic!("expected partial lineage, got {other:?}"),
    }
}

fn read_names(statement: &StatementAnalysis) -> Vec<&str> {
    statement
        .read
        .iter()
        .map(|relation| relation.name.value.as_str())
        .collect()
}

struct UnorderedTable {
    entry: Relation,
    columns: Vec<CatalogColumn>,
}

impl UnorderedTable {
    fn new(table: &str, columns: &[&str]) -> Self {
        Self {
            entry: relation(table),
            columns: columns
                .iter()
                .map(|column| CatalogColumn {
                    name: Ident::unquoted(*column),
                    ordinal: None,
                    data_type: None,
                    fields: None,
                })
                .collect(),
        }
    }
}

impl Catalog for UnorderedTable {
    fn lookup(&self, query: &Relation) -> Option<CatalogEntry<'_>> {
        (query == &self.entry).then(|| CatalogEntry {
            relation: self.entry.clone(),
            columns: Cow::Borrowed(&self.columns),
            coverage: SchemaCoverage::Exhaustive,
            order: SchemaOrder::Unknown,
            unknown_segments: Cow::Borrowed(&[]),
            evidence: Cow::Owned(vec![EvidenceRef("catalog:unordered".to_string())]),
        })
    }
}

#[test]
fn except_takes_values_from_the_left_operand_only() {
    for dialect in [Dialect::Generic, Dialect::PostgreSql] {
        let statement = analyze_in(dialect, "SELECT a FROM t1 EXCEPT SELECT b FROM t2");
        assert_eq!(statement.output.shape, Shape::Closed);
        assert_complete(&statement, 0, &[source("t1", "a")]);
        assert_eq!(read_names(&statement), ["t1", "t2"]);
    }
}

#[test]
fn each_operator_and_quantifier_supplies_values_by_its_rule() {
    let both = [source("t1", "a"), source("t2", "b")];
    let left = [source("t1", "a")];
    let cases: [(&str, &[SourceColumn]); 9] = [
        ("UNION", &both),
        ("UNION ALL", &both),
        ("UNION DISTINCT", &both),
        ("INTERSECT", &both),
        ("INTERSECT ALL", &both),
        ("INTERSECT DISTINCT", &both),
        ("EXCEPT", &left),
        ("EXCEPT ALL", &left),
        ("EXCEPT DISTINCT", &left),
    ];
    for (operator, expected) in cases {
        let sql = format!("SELECT a FROM t1 {operator} SELECT b FROM t2");
        let statement = analyze(&sql);
        assert_complete(&statement, 0, expected);
        assert_eq!(read_names(&statement), ["t1", "t2"], "{sql}");
    }
}

#[test]
fn unknown_star_on_the_right_of_except_leaves_the_left_value_complete() {
    for sql in [
        "SELECT a FROM t1 EXCEPT SELECT * FROM unknown_source",
        "SELECT a FROM t1 EXCEPT ALL SELECT * FROM unknown_source",
        "SELECT a FROM t1 EXCEPT SELECT u.* FROM unknown_source u",
    ] {
        let statement = analyze(sql);
        assert_complete(&statement, 0, &[source("t1", "a")]);
        assert_eq!(statement.output.shape, Shape::Closed, "{sql}");
    }
    // The same operand does make the value of a UNION partial.
    let union = analyze("SELECT a FROM t1 UNION SELECT * FROM unknown_source");
    assert!(matches!(
        union.output.columns[0].lineage,
        Lineage::Partial { .. }
    ));
}

#[test]
fn unbound_name_on_the_right_of_except_leaves_the_left_value_complete() {
    let statement = analyze("SELECT a FROM t1 EXCEPT SELECT nope FROM (SELECT x FROM t2) s");
    assert_complete(&statement, 0, &[source("t1", "a")]);
    assert_eq!(read_names(&statement), ["t1", "t2"]);
}

#[test]
fn unsupported_sources_on_the_right_of_except_do_not_reach_the_values() {
    for right in ["VALUES (1)", "TABLE t2"] {
        let statement = analyze(&format!("SELECT a FROM t1 EXCEPT {right}"));
        assert_complete(&statement, 0, &[source("t1", "a")]);
        let union = analyze(&format!("SELECT a FROM t1 UNION {right}"));
        let (_, unresolved) = partial(&union, 0);
        assert!(
            unresolved
                .iter()
                .any(|reason| matches!(reason, Unresolved::Unsupported { .. })),
            "{right}"
        );
    }
}

#[test]
fn unsupported_sources_on_the_left_of_except_stay() {
    let statement = analyze("VALUES (1) EXCEPT SELECT a FROM t1");
    let Shape::Open { unexpanded } = &statement.output.shape else {
        panic!("expected an open shape");
    };
    assert!(unexpanded.iter().any(|star| matches!(
        &star.lineage,
        Lineage::Partial { unresolved, .. }
            if unresolved.iter().any(|reason| matches!(
                reason,
                Unresolved::Unsupported { what: "VALUES source" }
            ))
    )));
}

#[test]
fn unbound_name_on_the_left_of_except_stays_without_right_candidates() {
    let statement = analyze("SELECT nope FROM (SELECT x FROM t1) s EXCEPT SELECT b FROM t2");
    let (known, unresolved) = partial(&statement, 0);
    assert!(known.is_empty());
    assert_eq!(
        unresolved,
        [Unresolved::Unbound {
            column: Ident::unquoted("nope")
        }]
    );
}

#[test]
fn unknown_star_on_the_left_of_except_stays_without_right_candidates() {
    let statement = analyze("SELECT a FROM (SELECT * FROM u EXCEPT SELECT b FROM t2) s");
    let (known, unresolved) = partial(&statement, 0);
    assert!(known.is_empty());
    assert_eq!(
        unresolved,
        [Unresolved::NamedBehindStar {
            relation: relation("u"),
            column: Ident::unquoted("a"),
        }]
    );
    assert_eq!(read_names(&statement), ["t2", "u"]);

    // A UNION in the same position records the right operand as a candidate.
    let union = analyze("SELECT a FROM (SELECT * FROM u UNION SELECT b FROM t2) s");
    let (_, unresolved) = partial(&union, 0);
    assert!(unresolved.iter().any(|reason| matches!(
        reason,
        Unresolved::AlignmentUnknown { possible_sources, .. }
            if possible_sources == &[source("t2", "b")]
    )));
}

#[test]
fn named_queries_through_an_except_unknown_segment_use_the_left_operand() {
    let statement =
        analyze("WITH e AS (SELECT * FROM u EXCEPT ALL SELECT b FROM t2) SELECT a FROM e");
    let (known, unresolved) = partial(&statement, 0);
    assert!(known.is_empty());
    assert!(
        unresolved
            .iter()
            .all(|reason| !matches!(reason, Unresolved::AlignmentUnknown { .. })),
        "{unresolved:?}"
    );
}

#[test]
fn a_star_over_an_except_with_an_unknown_left_star_has_left_contributions_only() {
    let statement = analyze("SELECT * FROM (SELECT * FROM u EXCEPT SELECT b FROM t2) s");
    assert!(statement.output.columns.is_empty());
    let Shape::Open { unexpanded } = &statement.output.shape else {
        panic!("expected an open shape");
    };
    assert_eq!(unexpanded.len(), 1);
    assert_eq!(
        unexpanded[0].lineage,
        Lineage::Partial {
            known: vec![],
            unresolved: vec![Unresolved::BehindStar {
                relation: relation("u")
            }],
        }
    );
}

#[test]
fn unordered_schemas_on_the_right_of_except_do_not_make_the_left_partial() {
    let catalog = UnorderedTable::new("t", &["z", "a"]);
    let analyzer = Analyzer::new(Dialect::Generic).with_catalog(&catalog);
    let statement = analyzer
        .analyze("SELECT p, q FROM s EXCEPT ALL SELECT * FROM t")
        .unwrap()
        .into_single()
        .unwrap();
    assert_complete(&statement, 0, &[source("s", "p")]);
    assert_complete(&statement, 1, &[source("s", "q")]);

    let statement = analyzer
        .analyze("SELECT p, q FROM s INTERSECT ALL SELECT * FROM t")
        .unwrap()
        .into_single()
        .unwrap();
    for column in &statement.output.columns {
        assert!(matches!(column.lineage, Lineage::Partial { .. }));
    }
}

#[test]
fn unordered_schemas_on_the_left_of_except_add_no_right_candidates() {
    let catalog = UnorderedTable::new("t", &["z", "a"]);
    let statement = Analyzer::new(Dialect::Generic)
        .with_catalog(&catalog)
        .analyze("SELECT * FROM t EXCEPT ALL SELECT p, q FROM s")
        .unwrap()
        .into_single()
        .unwrap();
    assert_eq!(statement.output.columns.len(), 2);
    assert_complete(&statement, 0, &[source("t", "z")]);
    assert_complete(&statement, 1, &[source("t", "a")]);
}

#[test]
fn the_right_operand_of_except_is_still_read_and_its_width_checked() {
    let statement = analyze("SELECT a FROM t1 EXCEPT SELECT b, c FROM t2");
    assert_eq!(read_names(&statement), ["t1", "t2"]);
    assert!(statement.diagnostics.iter().any(|diagnostic| matches!(
        diagnostic.kind,
        DiagnosticKind::SetOperationArityMismatch { left: 1, right: 2 }
    )));
}

#[test]
fn an_except_nested_on_the_left_of_a_union_supplies_its_left_values() {
    let statement = analyze("SELECT a FROM t1 EXCEPT SELECT b FROM t2 UNION SELECT c FROM t3");
    assert_complete(&statement, 0, &[source("t1", "a"), source("t3", "c")]);
    assert_eq!(read_names(&statement), ["t1", "t2", "t3"]);

    let statement = analyze(
        "SELECT a FROM (SELECT a FROM t1 EXCEPT SELECT b FROM t2) e UNION SELECT c FROM t3",
    );
    assert_complete(&statement, 0, &[source("t1", "a"), source("t3", "c")]);
}

#[test]
fn an_except_nested_on_the_right_of_a_union_supplies_its_left_values() {
    let statement = analyze("SELECT a FROM t1 UNION (SELECT b FROM t2 EXCEPT SELECT c FROM t3)");
    assert_complete(&statement, 0, &[source("t1", "a"), source("t2", "b")]);
    assert_eq!(read_names(&statement), ["t1", "t2", "t3"]);
}

#[test]
fn a_set_operation_on_the_right_of_except_supplies_no_values() {
    for inner in ["UNION", "INTERSECT"] {
        let statement = analyze(&format!(
            "SELECT a FROM t1 EXCEPT (SELECT b FROM t2 {inner} SELECT c FROM t3)"
        ));
        assert_complete(&statement, 0, &[source("t1", "a")]);
        assert_eq!(read_names(&statement), ["t1", "t2", "t3"]);
    }
}

#[test]
fn chained_excepts_keep_only_the_first_operand() {
    let statement = analyze("SELECT a FROM t1 EXCEPT SELECT b FROM t2 EXCEPT SELECT c FROM t3");
    assert_complete(&statement, 0, &[source("t1", "a")]);
    assert_eq!(read_names(&statement), ["t1", "t2", "t3"]);
}

#[test]
fn union_keeps_its_alignment_reason_with_the_right_candidates() {
    let catalog = UnorderedTable::new("t", &["z", "a"]);
    let statement = Analyzer::new(Dialect::Generic)
        .with_catalog(&catalog)
        .analyze("SELECT p, q FROM s UNION SELECT * FROM t")
        .unwrap()
        .into_single()
        .unwrap();
    let (_, unresolved) = partial(&statement, 0);
    assert!(unresolved.iter().any(|reason| matches!(
        reason,
        Unresolved::AlignmentUnknown { operation, .. } if operation.operator == SetOperator::Union
    )));
}

#[test]
fn row_dependencies_are_reported_as_not_collected() {
    let sqls = [
        "SELECT a FROM t1",
        "SELECT a FROM t1 EXCEPT SELECT b FROM t2",
        "SELECT a FROM t1 UNION SELECT b FROM t2",
        "SELECT a FROM t1 EXCEPT SELECT b, c FROM t2",
        "CREATE TABLE x AS SELECT a FROM t1 EXCEPT SELECT b FROM t2",
        "DELETE FROM t1 WHERE a = 1",
    ];
    for sql in sqls {
        let analysis: Analysis = Analyzer::new(Dialect::Generic).analyze(sql).unwrap();
        for statement in &analysis.statements {
            assert_eq!(statement.row_dependencies, RowDependencies::NotCollected);
        }
        let json = serde_json::to_value(&analysis).unwrap();
        assert_eq!(
            json["statements"][0]["row_dependencies"],
            serde_json::json!("NotCollected"),
            "{sql}"
        );
    }
}
