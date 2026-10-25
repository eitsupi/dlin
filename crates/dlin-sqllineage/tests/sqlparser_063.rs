//! Lineage behaviour pinned per dialect for parser features whose parse tree
//! is sensitive to the SQL parser release: join associativity, operator
//! precedence, newer predicates, aggregate-call clauses and pipe syntax.
//!
//! Every test names its dialect explicitly; the generic dialect accepts a
//! superset of syntax and would hide dialect-specific parsing.

mod common;

use common::{concrete_sources, find_mapping};
use dlin_sqllineage::{
    AnalyzeOptions, AnalyzeResult, CatalogProvider, ColumnMapping, ColumnOrigin, Dialect,
    StatementType, TableRef, TransformKind, analyze,
};

fn run(sql: &str, dialect: Dialect) -> Result<Vec<AnalyzeResult>, dlin_sqllineage::ParseError> {
    analyze(
        sql,
        AnalyzeOptions {
            dialect,
            ..AnalyzeOptions::default()
        },
    )
}

fn lineage(sql: &str, dialect: Dialect) -> AnalyzeResult {
    run(sql, dialect)
        .unwrap_or_else(|e| panic!("{dialect:?}: `{sql}` should analyze: {e}"))
        .into_iter()
        .next()
        .expect("one statement")
}

fn sources(result: &AnalyzeResult, column: &str) -> Vec<(String, String)> {
    concrete_sources(find_mapping(&result.columns.mappings, column))
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = items
        .iter()
        .map(|(t, c)| ((*t).to_string(), (*c).to_string()))
        .collect();
    v.sort();
    v
}

fn input_names(result: &AnalyzeResult) -> Vec<String> {
    let mut v: Vec<String> = result
        .tables
        .inputs
        .iter()
        .map(|t| t.table.clone())
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

fn assert_three_way_join(result: &AnalyzeResult) {
    assert_eq!(input_names(result), vec!["a", "b", "c"]);
    assert_eq!(sources(result, "ax"), pairs(&[("a", "x")]));
    assert_eq!(sources(result, "bv"), pairs(&[("b", "y")]));
    assert_eq!(sources(result, "cz"), pairs(&[("c", "z")]));
    for column in ["ax", "bv", "cz"] {
        assert_eq!(
            find_mapping(&result.columns.mappings, column).transform,
            TransformKind::Direct
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

impl CatalogProvider for AbcCatalog {
    fn list_columns(&self, _table: &TableRef) -> Option<Vec<String>> {
        None
    }

    fn resolve_column(&self, column: &str, candidates: &[TableRef]) -> Option<TableRef> {
        let owner = match column {
            "x" => "a",
            "y" => "b",
            "z" => "c",
            _ => return None,
        };
        candidates.iter().find(|t| t.table == owner).cloned()
    }
}

#[test]
fn postgres_right_deep_join_unqualified_columns_use_catalog() {
    let result = analyze(
        "SELECT x, y, z FROM a JOIN b JOIN c ON b.id = c.id ON a.id = b.id",
        AnalyzeOptions {
            dialect: Dialect::PostgreSql,
            catalog: Some(Box::new(AbcCatalog)),
            ..AnalyzeOptions::default()
        },
    )
    .expect("analyze")
    .remove(0);
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
    let mapping = find_mapping(&result.columns.mappings, "x");
    match &mapping.sources[..] {
        [ColumnOrigin::Ambiguous { column, candidates }] => {
            assert_eq!(column, "x");
            let mut names: Vec<_> = candidates.iter().map(|t| t.table.as_str()).collect();
            names.sort_unstable();
            assert_eq!(names, vec!["a", "b", "c"]);
        }
        other => panic!("expected an ambiguous origin over a, b and c, got {other:?}"),
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
            find_mapping(&result.columns.mappings, column).transform,
            TransformKind::Expression
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
    assert_eq!(result.columns.mappings.len(), 1);
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
            find_mapping(&result.columns.mappings, "r3").transform,
            TransformKind::Conditional,
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
        find_mapping(&result.columns.mappings, "r").transform,
        TransformKind::Expression
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
    assert_eq!(result.columns.mappings.len(), 1);
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
            find_mapping(&result.columns.mappings, column).transform,
            TransformKind::Expression,
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
        find_mapping(&result.columns.mappings, "ok").transform,
        TransformKind::Expression
    );
}

#[test]
fn is_json_in_where_adds_no_sources() {
    let result = lineage("SELECT a FROM t WHERE b IS JSON", Dialect::PostgreSql);
    assert_eq!(result.columns.mappings.len(), 1);
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
    // Only dialects that accept a bare `IN` operand parse this; the others
    // reject it, which is equally acceptable for lineage.
    for dialect in [Dialect::Snowflake, Dialect::DuckDb, Dialect::ClickHouse] {
        if let Ok(results) = run("SELECT a IN b AS m FROM t", dialect) {
            let result = &results[0];
            assert_eq!(
                sources(result, "m"),
                pairs(&[("t", "a"), ("t", "b")]),
                "{dialect:?}"
            );
        }
    }
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
        let err = run(sql, Dialect::Redshift).expect_err("UNPIVOT expression must be rejected");
        assert!(
            err.message
                .contains("UNPIVOT on an expression is not supported"),
            "unexpected error: {err}"
        );
    }
}

#[test]
fn from_first_query_without_select_list_is_a_star() {
    for dialect in [Dialect::BigQuery, Dialect::DuckDb, Dialect::Databricks] {
        let result = lineage("FROM t", dialect);
        assert_eq!(input_names(&result), vec!["t"], "{dialect:?}");
        let mapping = find_mapping(&result.columns.mappings, "*");
        assert!(
            matches!(&mapping.sources[..], [ColumnOrigin::Wildcard { table }] if table.table == "t"),
            "{dialect:?}: expected a wildcard on t, got {:?}",
            mapping.sources
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
            find_mapping(&result.columns.mappings, "s").transform,
            TransformKind::Aggregation,
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
        let Ok(results) = run(
            "MERGE INTO target t USING source s ON t.id = s.id \
             WHEN NOT MATCHED THEN INSERT (id, v) VALUES (s.id, s.v) \
             WHEN MATCHED THEN DO NOTHING",
            dialect,
        ) else {
            continue;
        };
        let result = &results[0];
        assert_eq!(result.statement_type, StatementType::Merge, "{dialect:?}");
        assert_eq!(sources(result, "id"), pairs(&[("source", "id")]));
        assert_eq!(sources(result, "v"), pairs(&[("source", "v")]));
    }
}

#[test]
fn merge_update_wildcard_is_an_unexpanded_star_without_catalog() {
    let result = lineage(
        "MERGE INTO target t USING source s ON t.id = s.id \
         WHEN MATCHED THEN UPDATE SET *",
        Dialect::Databricks,
    );
    assert_eq!(result.statement_type, StatementType::Merge);
    assert_eq!(result.tables.output, Some(TableRef::new("target")));
    assert_eq!(input_names(&result), vec!["source"]);
    let mapping = find_mapping(&result.columns.mappings, "*");
    assert_wildcard_on_source(mapping);
    assert!(result.columns.has_unresolved_stars);
}

#[test]
fn merge_insert_wildcard_is_an_unexpanded_star_without_catalog() {
    let result = lineage(
        "MERGE INTO target t USING source s ON t.id = s.id \
         WHEN NOT MATCHED THEN INSERT *",
        Dialect::Databricks,
    );
    assert_eq!(result.statement_type, StatementType::Merge);
    let mapping = find_mapping(&result.columns.mappings, "*");
    assert_wildcard_on_source(mapping);
}

fn assert_wildcard_on_source(mapping: &ColumnMapping) {
    assert!(
        matches!(&mapping.sources[..], [ColumnOrigin::Wildcard { table }] if table.table == "source"),
        "expected a wildcard on source, got {:?}",
        mapping.sources
    );
}

struct SourceCatalog;

impl CatalogProvider for SourceCatalog {
    fn list_columns(&self, table: &TableRef) -> Option<Vec<String>> {
        (table.table == "source").then(|| vec!["id".into(), "val".into()])
    }

    fn resolve_column(&self, _column: &str, _candidates: &[TableRef]) -> Option<TableRef> {
        None
    }
}

#[test]
fn merge_wildcards_expand_from_catalog() {
    for action in ["UPDATE SET *", "INSERT *"] {
        let kind = if action.starts_with("UPDATE") {
            "MATCHED"
        } else {
            "NOT MATCHED"
        };
        let sql =
            format!("MERGE INTO target t USING source s ON t.id = s.id WHEN {kind} THEN {action}");
        let result = analyze(
            &sql,
            AnalyzeOptions {
                dialect: Dialect::Databricks,
                catalog: Some(Box::new(SourceCatalog)),
                ..AnalyzeOptions::default()
            },
        )
        .expect("analyze")
        .remove(0);
        assert_eq!(
            sources(&result, "id"),
            pairs(&[("source", "id")]),
            "{action}"
        );
        assert_eq!(
            sources(&result, "val"),
            pairs(&[("source", "val")]),
            "{action}"
        );
        assert!(!result.columns.has_unresolved_stars, "{action}");
    }
}

// ---------------------------------------------------------------------------
// Pipe operators are rejected rather than silently ignored.
// ---------------------------------------------------------------------------

fn assert_pipe_rejected(sql: &str, dialect: Dialect) {
    match run(sql, dialect) {
        Ok(results) => panic!("{dialect:?}: `{sql}` should be rejected, got {results:?}"),
        Err(err) => assert!(
            err.message.contains("pipe operators are not supported"),
            "{dialect:?}: `{sql}` failed with an unrelated error: {err}"
        ),
    }
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
