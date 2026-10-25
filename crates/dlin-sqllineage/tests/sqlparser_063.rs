//! Lineage behaviour pinned per dialect for parser features whose parse tree
//! is sensitive to the SQL parser release: join associativity, operator
//! precedence, newer predicates, aggregate-call clauses and pipe syntax.
//!
//! Every test names its dialect explicitly; the generic dialect accepts a
//! superset of syntax and would hide dialect-specific parsing.

mod common;

use common::{concrete_sources, find_mapping};
use dlin_sqllineage::{
    Analysis, Analyzer, Candidate, Catalog, CatalogColumn, CatalogEntry, DiagnosticKind, Dialect,
    Ident, Lineage, Relation, Severity, Shape, StatementAnalysis, StatementKind, Transform,
    Unresolved,
};
use std::borrow::Cow;

fn run(sql: &str, dialect: Dialect) -> Result<Analysis, dlin_sqllineage::ParseError> {
    Analyzer::new(dialect).analyze(sql)
}

fn lineage(sql: &str, dialect: Dialect) -> StatementAnalysis {
    run(sql, dialect)
        .unwrap_or_else(|e| panic!("{dialect:?}: `{sql}` should analyze: {e}"))
        .into_single()
        .expect("one statement")
}

fn sources(result: &StatementAnalysis, column: &str) -> Vec<(String, String)> {
    concrete_sources(find_mapping(&result.output.columns, column))
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = items
        .iter()
        .map(|(t, c)| ((*t).to_string(), (*c).to_string()))
        .collect();
    v.sort();
    v
}

fn input_names(result: &StatementAnalysis) -> Vec<String> {
    let mut v: Vec<String> = result
        .read
        .iter()
        .map(|relation| relation.name.value.clone())
        .collect();
    v.sort();
    v
}

// ---------------------------------------------------------------------------
// Chained joins whose ON clauses are written after the last join.
// ---------------------------------------------------------------------------

const NESTED_JOIN_SQL: &str = "SELECT a.x AS ax, b.y AS bv, c.z AS cz \
     FROM a JOIN b JOIN c ON b.id = c.id ON a.id = b.id";

const BARE_JOIN_CHAIN_SQL: &str = "SELECT a.x AS ax, b.y AS bv, c.z AS cz \
     FROM a JOIN b JOIN c ON c.id = a.id";

fn assert_three_way_join(result: &StatementAnalysis) {
    assert_eq!(input_names(result), vec!["a", "b", "c"]);
    assert_eq!(sources(result, "ax"), pairs(&[("a", "x")]));
    assert_eq!(sources(result, "bv"), pairs(&[("b", "y")]));
    assert_eq!(sources(result, "cz"), pairs(&[("c", "z")]));
    for column in ["ax", "bv", "cz"] {
        assert_eq!(
            find_mapping(&result.output.columns, column).transform,
            Transform::Direct
        );
    }
}

#[test]
fn mysql_join_chain_with_trailing_on_clauses() {
    assert_three_way_join(&lineage(NESTED_JOIN_SQL, Dialect::MySql));
}

#[test]
fn mysql_join_chain_with_bare_join_in_the_middle() {
    assert_three_way_join(&lineage(BARE_JOIN_CHAIN_SQL, Dialect::MySql));
}

#[test]
fn redshift_join_chain_with_trailing_on_clauses() {
    assert_three_way_join(&lineage(NESTED_JOIN_SQL, Dialect::Redshift));
}

#[test]
fn redshift_join_chain_with_bare_join_in_the_middle() {
    assert_three_way_join(&lineage(BARE_JOIN_CHAIN_SQL, Dialect::Redshift));
}

#[test]
fn postgres_right_deep_join_chain() {
    assert_three_way_join(&lineage(NESTED_JOIN_SQL, Dialect::PostgreSql));
}

#[test]
fn postgres_right_deep_join_chain_with_four_relations() {
    let result = lineage(
        "SELECT t0.a AS a, t1.b AS b, t2.c AS c, t3.d AS d \
         FROM t0 LEFT JOIN t1 JOIN t2 JOIN t3 ON t2.k = t3.k ON t1.k = t2.k ON t0.k = t1.k",
        Dialect::PostgreSql,
    );
    assert_eq!(input_names(&result), vec!["t0", "t1", "t2", "t3"]);
    assert_eq!(sources(&result, "a"), pairs(&[("t0", "a")]));
    assert_eq!(sources(&result, "b"), pairs(&[("t1", "b")]));
    assert_eq!(sources(&result, "c"), pairs(&[("t2", "c")]));
    assert_eq!(sources(&result, "d"), pairs(&[("t3", "d")]));
}

#[test]
fn postgres_right_deep_join_resolves_aliases() {
    let result = lineage(
        "SELECT x.a AS a, z.c AS c FROM t0 AS x JOIN t1 AS y JOIN t2 AS z ON y.k = z.k ON x.k = y.k",
        Dialect::PostgreSql,
    );
    assert_eq!(input_names(&result), vec!["t0", "t1", "t2"]);
    assert_eq!(sources(&result, "a"), pairs(&[("t0", "a")]));
    assert_eq!(sources(&result, "c"), pairs(&[("t2", "c")]));
}

struct AbcCatalog;

impl Catalog for AbcCatalog {
    fn lookup(&self, relation: &Relation) -> Option<CatalogEntry<'_>> {
        let valid = match relation.name.value.as_str() {
            "a" => vec!["x", "id"],
            "b" => vec!["y", "id"],
            "c" => vec!["z", "id"],
            _ => return None,
        };
        let columns = valid
            .iter()
            .map(|name| CatalogColumn {
                name: Ident::unquoted(*name),
                data_type: None,
                fields: None,
            })
            .collect();
        Some(CatalogEntry {
            relation: relation.clone(),
            columns: Cow::Owned(columns),
        })
    }
}

#[test]
fn postgres_right_deep_join_unqualified_columns_use_catalog() {
    let catalog = AbcCatalog;
    let result = Analyzer::new(Dialect::PostgreSql)
        .with_catalog(&catalog)
        .analyze("SELECT x, y, z FROM a JOIN b JOIN c ON b.id = c.id ON a.id = b.id")
        .expect("analyze")
        .into_single()
        .expect("one statement");
    assert_eq!(sources(&result, "x"), pairs(&[("a", "x")]));
    assert_eq!(sources(&result, "y"), pairs(&[("b", "y")]));
    assert_eq!(sources(&result, "z"), pairs(&[("c", "z")]));
}

#[test]
fn postgres_unqualified_column_over_nested_join_is_ambiguous_without_catalog() {
    let result = lineage(
        "SELECT x FROM a JOIN b JOIN c ON b.id = c.id ON a.id = b.id",
        Dialect::PostgreSql,
    );
    let mapping = find_mapping(&result.output.columns, "x");
    match &mapping.lineage {
        Lineage::Partial { unresolved, .. } => match unresolved.as_slice() {
            [Unresolved::Ambiguous { column, candidates }] => {
                assert_eq!(column.value, "x");
                let mut names: Vec<_> = candidates
                    .iter()
                    .map(|candidate| match candidate {
                        Candidate::Relation(relation) => relation.name.value.as_str(),
                        Candidate::Cte(name) => name.value.as_str(),
                        Candidate::Derived(name) => name
                            .as_ref()
                            .map_or("<derived>", |name| name.value.as_str()),
                    })
                    .collect();
                names.sort_unstable();
                assert_eq!(names, vec!["a", "b", "c"]);
            }
            other => panic!("expected an ambiguous origin over a, b and c, got {other:?}"),
        },
        other => panic!("expected partial lineage, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// SQLite operators: REGEXP / MATCH / GLOB.
// ---------------------------------------------------------------------------

#[test]
fn sqlite_regexp_match_glob_operands_are_sources() {
    let result = lineage(
        "SELECT a REGEXP b AS r, c MATCH d AS m, e GLOB f AS g FROM t",
        Dialect::SQLite,
    );
    assert_eq!(sources(&result, "r"), pairs(&[("t", "a"), ("t", "b")]));
    assert_eq!(sources(&result, "m"), pairs(&[("t", "c"), ("t", "d")]));
    assert_eq!(sources(&result, "g"), pairs(&[("t", "e"), ("t", "f")]));
    for column in ["r", "m", "g"] {
        assert_eq!(
            find_mapping(&result.output.columns, column).transform,
            Transform::Expression
        );
    }
}

#[test]
fn sqlite_regexp_and_glob_combine_with_boolean_operators() {
    let result = lineage(
        "SELECT a REGEXP b AND c AS r, d GLOB e OR f AS g, g1 GLOB 'x*' AND h AS lit FROM t",
        Dialect::SQLite,
    );
    assert_eq!(
        sources(&result, "r"),
        pairs(&[("t", "a"), ("t", "b"), ("t", "c")])
    );
    assert_eq!(
        sources(&result, "g"),
        pairs(&[("t", "d"), ("t", "e"), ("t", "f")])
    );
    assert_eq!(sources(&result, "lit"), pairs(&[("t", "g1"), ("t", "h")]));
}

#[test]
fn sqlite_glob_in_where_does_not_become_a_source() {
    let result = lineage(
        "SELECT a FROM t WHERE b GLOB 'x*' AND c REGEXP d",
        Dialect::SQLite,
    );
    assert_eq!(input_names(&result), vec!["t"]);
    assert_eq!(result.output.columns.len(), 1);
    assert_eq!(sources(&result, "a"), pairs(&[("t", "a")]));
}

#[test]
fn mysql_regexp_operands_are_sources() {
    let result = lineage(
        "SELECT a REGEXP b AND c AS r, d RLIKE e AS rl FROM t",
        Dialect::MySql,
    );
    assert_eq!(
        sources(&result, "r"),
        pairs(&[("t", "a"), ("t", "b"), ("t", "c")])
    );
    assert_eq!(sources(&result, "rl"), pairs(&[("t", "d"), ("t", "e")]));
}

// ---------------------------------------------------------------------------
// Precedence: IS DISTINCT FROM and MySQL DIV.
// ---------------------------------------------------------------------------

#[test]
fn is_distinct_from_with_boolean_operators_keeps_every_operand() {
    for dialect in [
        Dialect::PostgreSql,
        Dialect::Snowflake,
        Dialect::DuckDb,
        Dialect::Redshift,
        Dialect::BigQuery,
    ] {
        let result = lineage(
            "SELECT a IS DISTINCT FROM b AND c AS r1, \
                    a IS NOT DISTINCT FROM b OR c AS r2, \
                    CASE WHEN a IS DISTINCT FROM b THEN d ELSE e END AS r3 \
             FROM t",
            dialect,
        );
        assert_eq!(
            sources(&result, "r1"),
            pairs(&[("t", "a"), ("t", "b"), ("t", "c")]),
            "{dialect:?}"
        );
        assert_eq!(
            sources(&result, "r2"),
            pairs(&[("t", "a"), ("t", "b"), ("t", "c")]),
            "{dialect:?}"
        );
        assert_eq!(
            sources(&result, "r3"),
            pairs(&[("t", "a"), ("t", "b"), ("t", "d"), ("t", "e")]),
            "{dialect:?}"
        );
        assert_eq!(
            find_mapping(&result.output.columns, "r3").transform,
            Transform::Conditional,
            "{dialect:?}"
        );
    }
}

#[test]
fn is_distinct_from_with_arithmetic_operands() {
    let result = lineage(
        "SELECT a + 1 IS DISTINCT FROM b * 2 AS r FROM t",
        Dialect::PostgreSql,
    );
    assert_eq!(sources(&result, "r"), pairs(&[("t", "a"), ("t", "b")]));
    assert_eq!(
        find_mapping(&result.output.columns, "r").transform,
        Transform::Expression
    );
}

#[test]
fn is_distinct_from_in_where_and_join_conditions_adds_no_sources() {
    let result = lineage(
        "SELECT t1.a AS a FROM t1 JOIN t2 ON t1.k IS NOT DISTINCT FROM t2.k \
         WHERE t1.b IS DISTINCT FROM t2.b AND t2.c > 0",
        Dialect::PostgreSql,
    );
    assert_eq!(input_names(&result), vec!["t1", "t2"]);
    assert_eq!(result.output.columns.len(), 1);
    assert_eq!(sources(&result, "a"), pairs(&[("t1", "a")]));
}

#[test]
fn mysql_div_precedence_keeps_every_operand() {
    let result = lineage(
        "SELECT a DIV b + c AS r1, a + b DIV c AS r2, a DIV b AND c AS r3 FROM t",
        Dialect::MySql,
    );
    for column in ["r1", "r2", "r3"] {
        assert_eq!(
            sources(&result, column),
            pairs(&[("t", "a"), ("t", "b"), ("t", "c")]),
            "{column}"
        );
        assert_eq!(
            find_mapping(&result.output.columns, column).transform,
            Transform::Expression,
            "{column}"
        );
    }
}

#[test]
fn mysql_div_with_literal_keeps_column_operands_only() {
    let result = lineage(
        "SELECT a DIV 2 AS half, 10 DIV b AS q FROM t",
        Dialect::MySql,
    );
    assert_eq!(sources(&result, "half"), pairs(&[("t", "a")]));
    assert_eq!(sources(&result, "q"), pairs(&[("t", "b")]));
}

// ---------------------------------------------------------------------------
// Predicates and expressions added by newer parser releases.
// ---------------------------------------------------------------------------

#[test]
fn is_json_predicate_operand_is_a_source() {
    let result = lineage(
        "SELECT payload IS JSON OBJECT WITH UNIQUE KEYS AS ok, \
                doc IS NOT JSON AS bad, \
                CASE WHEN body IS JSON ARRAY THEN id END AS picked \
         FROM t",
        Dialect::PostgreSql,
    );
    assert_eq!(sources(&result, "ok"), pairs(&[("t", "payload")]));
    assert_eq!(sources(&result, "bad"), pairs(&[("t", "doc")]));
    assert_eq!(
        sources(&result, "picked"),
        pairs(&[("t", "body"), ("t", "id")])
    );
    assert_eq!(
        find_mapping(&result.output.columns, "ok").transform,
        Transform::Expression
    );
}

#[test]
fn is_json_in_where_adds_no_sources() {
    let result = lineage("SELECT a FROM t WHERE b IS JSON", Dialect::PostgreSql);
    assert_eq!(result.output.columns.len(), 1);
    assert_eq!(sources(&result, "a"), pairs(&[("t", "a")]));
}

#[test]
fn postgres_aliased_function_arguments_keep_their_operands() {
    let result = lineage(
        "SELECT XMLFOREST(a AS x, b AS y) AS doc FROM t",
        Dialect::PostgreSql,
    );
    assert_eq!(sources(&result, "doc"), pairs(&[("t", "a"), ("t", "b")]));
}

#[test]
fn like_escape_expression_is_a_source() {
    let result = lineage("SELECT a LIKE b ESCAPE c AS m FROM t", Dialect::PostgreSql);
    assert_eq!(
        sources(&result, "m"),
        pairs(&[("t", "a"), ("t", "b"), ("t", "c")])
    );
    let literal = lineage(
        "SELECT a LIKE b ESCAPE '!' AS m FROM t",
        Dialect::PostgreSql,
    );
    assert_eq!(sources(&literal, "m"), pairs(&[("t", "a"), ("t", "b")]));
}

#[test]
fn unparenthesized_in_operand_is_a_source() {
    // Only ClickHouse accepts a bare `IN` operand; the other dialects reject
    // it at parse time. Pin the exact set so the test cannot pass vacuously.
    let mut accepted = Vec::new();
    for dialect in [Dialect::Snowflake, Dialect::DuckDb, Dialect::ClickHouse] {
        let Ok(results) = run("SELECT a IN b AS m FROM t", dialect) else {
            continue;
        };
        assert_eq!(
            sources(&results.statements[0], "m"),
            pairs(&[("t", "a"), ("t", "b")]),
            "{dialect:?}"
        );
        accepted.push(format!("{dialect:?}"));
    }
    assert_eq!(accepted, vec!["ClickHouse"]);
}

#[test]
fn postgres_order_by_using_operator_keeps_projection_lineage() {
    let result = lineage("SELECT a, b FROM t ORDER BY a USING <", Dialect::PostgreSql);
    assert_eq!(sources(&result, "a"), pairs(&[("t", "a")]));
    assert_eq!(sources(&result, "b"), pairs(&[("t", "b")]));
}

#[test]
fn redshift_unpivot_expression_is_rejected() {
    // The unpivoted value is derived from the expression rather than from a
    // physical column, so resolving `v` against `t` would invent `t.v`.
    for sql in [
        "SELECT t.id AS id FROM t, UNPIVOT t.attrs AS v AT k",
        "SELECT v FROM t, UNPIVOT t.attrs AS v AT k",
    ] {
        let result = lineage(sql, Dialect::Redshift);
        assert!(result.output.columns.is_empty(), "{sql}");
        assert!(
            result.diagnostics.iter().any(|diagnostic| {
                diagnostic.severity == Severity::Error
                    && diagnostic.kind
                        == DiagnosticKind::UnsupportedSyntax {
                            what: "UNPIVOT on an expression",
                        }
            }),
            "{sql}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn from_first_query_without_select_list_is_a_star() {
    for dialect in [Dialect::BigQuery, Dialect::DuckDb, Dialect::Databricks] {
        let result = lineage("FROM t", dialect);
        assert_eq!(input_names(&result), vec!["t"], "{dialect:?}");
        assert!(
            matches!(&result.output.shape, Shape::Open { unexpanded } if unexpanded.iter().any(|star| star.relation.as_ref().is_some_and(|r| r.name.value == "t"))),
            "{dialect:?}: expected an unexpanded wildcard on t, got {:?}",
            result.output.shape
        );
    }
}

#[test]
fn from_first_query_with_select_list_keeps_projection_lineage() {
    for dialect in [Dialect::DuckDb, Dialect::Databricks] {
        let result = lineage("FROM t SELECT a, b + c AS d", dialect);
        assert_eq!(sources(&result, "a"), pairs(&[("t", "a")]), "{dialect:?}");
        assert_eq!(
            sources(&result, "d"),
            pairs(&[("t", "b"), ("t", "c")]),
            "{dialect:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Aggregate call clauses are not lineage sources.
// ---------------------------------------------------------------------------

#[test]
fn aggregate_inline_where_clause_is_not_a_source() {
    for dialect in [Dialect::BigQuery, Dialect::PostgreSql, Dialect::Snowflake] {
        let result = lineage(
            "SELECT SUM(x WHERE c > 0) AS s, ARRAY_AGG(y WHERE d IS NOT NULL ORDER BY o) AS arr FROM t",
            dialect,
        );
        assert_eq!(sources(&result, "s"), pairs(&[("t", "x")]), "{dialect:?}");
        assert_eq!(
            find_mapping(&result.output.columns, "s").transform,
            Transform::Aggregation,
            "{dialect:?}"
        );
        assert_eq!(sources(&result, "arr"), pairs(&[("t", "y")]), "{dialect:?}");
    }
}

#[test]
fn aggregate_filter_clause_is_not_a_source() {
    let result = lineage(
        "SELECT SUM(x) FILTER (WHERE c > 0) AS s FROM t",
        Dialect::PostgreSql,
    );
    assert_eq!(sources(&result, "s"), pairs(&[("t", "x")]));
}

// ---------------------------------------------------------------------------
// MERGE shapes.
// ---------------------------------------------------------------------------

#[test]
fn merge_do_nothing_clause_contributes_no_lineage() {
    for dialect in [Dialect::PostgreSql, Dialect::Snowflake, Dialect::MsSql] {
        let result = lineage(
            "MERGE INTO target t USING source s ON t.id = s.id \
             WHEN NOT MATCHED THEN INSERT (id, v) VALUES (s.id, s.v) \
             WHEN MATCHED THEN DO NOTHING",
            dialect,
        );
        assert_eq!(result.kind, StatementKind::Merge, "{dialect:?}");
        assert_eq!(
            sources(&result, "id"),
            pairs(&[("source", "id")]),
            "{dialect:?}"
        );
        assert_eq!(
            sources(&result, "v"),
            pairs(&[("source", "v")]),
            "{dialect:?}"
        );
    }
}

#[test]
fn merge_update_wildcard_is_an_unexpanded_star_without_catalog() {
    let result = lineage(
        "MERGE INTO target t USING source s ON t.id = s.id \
         WHEN MATCHED THEN UPDATE SET *",
        Dialect::Databricks,
    );
    assert_eq!(result.kind, StatementKind::Merge);
    assert_eq!(result.written, Some(common::table("target")));
    assert_eq!(input_names(&result), vec!["source"]);
    assert_wildcard_on_source(&result);
    assert!(result.has_unresolved_stars);
}

#[test]
fn merge_insert_wildcard_is_an_unexpanded_star_without_catalog() {
    let result = lineage(
        "MERGE INTO target t USING source s ON t.id = s.id \
         WHEN NOT MATCHED THEN INSERT *",
        Dialect::Databricks,
    );
    assert_eq!(result.kind, StatementKind::Merge);
    assert_wildcard_on_source(&result);
}

#[test]
fn merge_insert_row_is_an_unexpanded_star_without_catalog() {
    let result = lineage(
        "MERGE INTO target t USING source s ON t.id = s.id \
         WHEN NOT MATCHED THEN INSERT ROW",
        Dialect::BigQuery,
    );
    assert_eq!(result.kind, StatementKind::Merge);
    assert_eq!(result.written, Some(common::table("target")));
    assert_eq!(input_names(&result), vec!["source"]);
    assert_wildcard_on_source(&result);
    assert!(result.has_unresolved_stars);
}

/// The merge's only output is an unexpanded star over the `source` relation.
fn assert_wildcard_on_source(result: &StatementAnalysis) {
    assert!(result.output.columns.is_empty());
    let Shape::Open { unexpanded } = &result.output.shape else {
        panic!(
            "expected an unexpanded wildcard, got {:?}",
            result.output.shape
        )
    };
    assert_eq!(unexpanded.len(), 1);
    assert_eq!(unexpanded[0].after_ordinal, None);
    let source = common::table("source");
    assert_eq!(unexpanded[0].relation.as_ref(), Some(&source));
    assert_eq!(
        unexpanded[0].lineage,
        Lineage::Partial {
            known: Vec::new(),
            unresolved: vec![Unresolved::BehindStar { relation: source }],
        }
    );
}

struct SourceCatalog;

impl Catalog for SourceCatalog {
    fn lookup(&self, relation: &Relation) -> Option<CatalogEntry<'_>> {
        (relation.name.value == "source").then(|| CatalogEntry {
            relation: relation.clone(),
            columns: Cow::Owned(
                ["id", "val"]
                    .into_iter()
                    .map(|name| CatalogColumn {
                        name: Ident::unquoted(name),
                        data_type: None,
                        fields: None,
                    })
                    .collect(),
            ),
        })
    }
}

#[test]
fn merge_wildcards_expand_from_catalog() {
    // `INSERT ROW` is BigQuery syntax; Databricks rejects it.
    for (dialect, clause, action) in [
        (Dialect::Databricks, "MATCHED", "UPDATE SET *"),
        (Dialect::Databricks, "NOT MATCHED", "INSERT *"),
        (Dialect::BigQuery, "NOT MATCHED", "INSERT ROW"),
    ] {
        let sql = format!(
            "MERGE INTO target t USING source s ON t.id = s.id WHEN {clause} THEN {action}"
        );
        let catalog = SourceCatalog;
        let result = Analyzer::new(dialect)
            .with_catalog(&catalog)
            .analyze(&sql)
            .expect("analyze")
            .into_single()
            .expect("one statement");
        assert_eq!(
            sources(&result, "id"),
            pairs(&[("source", "id")]),
            "{dialect:?}: {action}"
        );
        assert_eq!(
            sources(&result, "val"),
            pairs(&[("source", "val")]),
            "{dialect:?}: {action}"
        );
        assert_eq!(result.output.shape, Shape::Closed, "{dialect:?}: {action}");
        assert!(!result.has_unresolved_stars, "{dialect:?}: {action}");
    }
}

// ---------------------------------------------------------------------------
// Pipe operators are rejected rather than silently ignored.
// ---------------------------------------------------------------------------

fn assert_pipe_rejected(sql: &str, dialect: Dialect) {
    let result = lineage(sql, dialect);
    assert!(result.output.columns.is_empty(), "{dialect:?}: `{sql}`");
    assert!(
        result.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error
                && diagnostic.kind
                    == DiagnosticKind::UnsupportedSyntax {
                        what: "pipe operators",
                    }
        }),
        "{dialect:?}: `{sql}` diagnostics: {:?}",
        result.diagnostics
    );
}

#[test]
fn pipe_operator_query_is_rejected() {
    assert_pipe_rejected("FROM t |> SELECT a", Dialect::BigQuery);
    assert_pipe_rejected("SELECT * FROM t |> WHERE a > 1", Dialect::BigQuery);
    assert_pipe_rejected("FROM t |> SELECT a", Dialect::Databricks);
}

#[test]
fn pipe_operator_in_cte_is_rejected() {
    assert_pipe_rejected(
        "WITH c AS (FROM t |> SELECT a) SELECT a FROM c",
        Dialect::BigQuery,
    );
}

#[test]
fn pipe_operator_in_derived_table_is_rejected() {
    assert_pipe_rejected(
        "SELECT s.a FROM (FROM t |> SELECT a) AS s",
        Dialect::BigQuery,
    );
}

#[test]
fn pipe_operator_in_scalar_and_predicate_subqueries_is_rejected() {
    assert_pipe_rejected(
        "SELECT (SELECT a FROM t |> LIMIT 1) AS v FROM u",
        Dialect::BigQuery,
    );
    assert_pipe_rejected(
        "SELECT b FROM u WHERE b IN (SELECT a FROM t |> WHERE a > 1)",
        Dialect::BigQuery,
    );
    assert_pipe_rejected(
        "SELECT b FROM u WHERE EXISTS (SELECT a FROM t |> WHERE a > 1)",
        Dialect::BigQuery,
    );
}

#[test]
fn pipe_operator_in_statement_sources_is_rejected() {
    assert_pipe_rejected("INSERT INTO dst FROM t |> SELECT a", Dialect::BigQuery);
    assert_pipe_rejected("CREATE TABLE dst AS FROM t |> SELECT a", Dialect::BigQuery);
    assert_pipe_rejected(
        "SELECT a FROM u UNION ALL (FROM t |> SELECT a)",
        Dialect::BigQuery,
    );
}

#[test]
fn query_without_pipe_operator_is_unaffected() {
    let result = lineage("SELECT a FROM t WHERE a > 1 | 2", Dialect::BigQuery);
    assert_eq!(sources(&result, "a"), pairs(&[("t", "a")]));
}
