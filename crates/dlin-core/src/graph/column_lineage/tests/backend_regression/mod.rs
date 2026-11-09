//! Regression tests pinning column-lineage results and messages for a
//! representative set of SQL shapes: a plain `SELECT`, a CTE, `SELECT *` with
//! a known and an unknown schema, top-level `UNION ALL`/`EXCEPT`, set
//! operations whose outputs cannot be established, an unresolvable column,
//! and unparseable SQL.
//!
//! Every case builds a manifest with one model whose compiled SQL is the SQL
//! under test, plus optional upstream models that supply the schemas of the
//! tables the SQL reads, and then runs the production
//! [`compute_column_lineage`] over it. Assertions are made on the public
//! [`ModelColumnLineage`]: the output slots (sources and transformation) and
//! the errors (kind, column, message and hint). The output is what the SQL
//! says; columns that the model's YAML declares and the SQL does not list are
//! reported as non-fatal diagnostics and never become outputs.
//!
//! Two regressions are pinned here. Top-level `UNION`/`INTERSECT`/`EXCEPT`
//! statements are lineage-bearing, so their columns are traced rather than
//! rejected as "not lineage-bearing". Unparseable SQL is reported as a parse
//! failure rather than as an indeterminate column-resolution failure.

use std::collections::HashMap;

use super::super::{
    CatalogSnapshot, ColumnLineageCache, ColumnLineageError, ColumnLineageErrorKind, ColumnSource,
    DlinDialect, Ident, ModelColumnLineage, OutputName, OutputShape, OutputSlot, Quote, SlotLookup,
    TransformationType, compute_column_lineage, debug_trace_column_json,
};
use super::{ModelLineageTestExt, SlotTestExt, assert_counts, assert_declared_difference};
use crate::parser::manifest::{DependsOn, Manifest, ManifestColumn, ManifestConfig, ManifestNode};

const ROOT_MODEL: &str = "root";
const PARSE_HINT: &str =
    "Check the SQL with `dlin debug parse-sql`; ensure the correct --dialect is set";
const STAR_HINT: &str = "column may be introduced via SELECT * that could not be expanded; \
                         define upstream columns in the model YAML to enable full resolution";

/// Upstream models as `(name, YAML columns)` pairs. The root model reads
/// them, so their columns become the catalog its SQL is analyzed against.
type Upstream<'a> = &'a [(&'a str, &'a [&'a str])];

fn manifest_columns(names: &[&str]) -> indexmap::IndexMap<String, ManifestColumn> {
    names
        .iter()
        .map(|name| {
            (
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                    quote: None,
                },
            )
        })
        .collect()
}

fn make_node(
    name: &str,
    depends_on: Vec<String>,
    columns: &[&str],
    compiled_code: Option<&str>,
) -> ManifestNode {
    ManifestNode {
        unique_id: format!("model.proj.{name}"),
        name: name.to_string(),
        alias: None,
        resource_type: "model".to_string(),
        depends_on: DependsOn { nodes: depends_on },
        config: ManifestConfig::default(),
        description: None,
        path: None,
        original_file_path: None,
        columns: manifest_columns(columns),
        compiled_code: compiled_code.map(str::to_string),
        database: None,
        schema: None,
    }
}

/// Build a manifest with a single "root" model carrying `compiled_code` and
/// the given YAML columns, plus the upstream models it depends on.
fn make_manifest(compiled_code: &str, upstream: Upstream<'_>, yaml_columns: &[&str]) -> Manifest {
    let mut nodes = HashMap::new();
    let mut depends_on = Vec::new();
    for (name, columns) in upstream {
        let node = make_node(name, vec![], columns, None);
        depends_on.push(node.unique_id.clone());
        nodes.insert(node.unique_id.clone(), node);
    }
    let root = make_node(ROOT_MODEL, depends_on, yaml_columns, Some(compiled_code));
    nodes.insert(root.unique_id.clone(), root);
    Manifest {
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        ..Default::default()
    }
}

/// Run the production column-lineage path over the root model.
fn analyze(
    sql: &str,
    dialect: DlinDialect,
    upstream: Upstream<'_>,
    yaml_columns: &[&str],
) -> ModelColumnLineage {
    let manifest = make_manifest(sql, upstream, yaml_columns);
    compute_column_lineage(
        &manifest,
        ROOT_MODEL,
        dialect,
        &mut ColumnLineageCache::disabled(),
    )
}

/// Analyze `sql` with upstream models defined by SQL (`(name, SQL)` pairs).
/// Each upstream model provides the output of its SQL, in SQL order.
fn analyze_over_sql_models(
    sql: &str,
    dialect: DlinDialect,
    upstream: &[(&str, &str)],
) -> ModelColumnLineage {
    let mut manifest = make_manifest(sql, &[], &[]);
    let mut depends_on = Vec::new();
    for (name, upstream_sql) in upstream {
        let node = make_node(name, vec![], &[], Some(upstream_sql));
        depends_on.push(node.unique_id.clone());
        manifest.nodes.insert(node.unique_id.clone(), node);
    }
    manifest
        .nodes
        .get_mut(&format!("model.proj.{ROOT_MODEL}"))
        .unwrap()
        .depends_on
        .nodes = depends_on;
    compute_column_lineage(
        &manifest,
        ROOT_MODEL,
        dialect,
        &mut ColumnLineageCache::disabled(),
    )
}

/// Analyze `sql` with the generic dialect, no upstream models and the given
/// YAML columns.
fn generic(sql: &str, yaml_columns: &[&str]) -> ModelColumnLineage {
    analyze(sql, DlinDialect::Generic, &[], yaml_columns)
}

fn entry<'a>(result: &'a ModelColumnLineage, column: &str) -> &'a OutputSlot<ColumnSource> {
    result.slot_named(column)
}

fn source_pairs(entry: &OutputSlot<ColumnSource>) -> Vec<(String, String)> {
    entry
        .lineage
        .known()
        .iter()
        .map(|source| (source.table.clone(), source.column.to_string()))
        .collect()
}

fn assert_source_pairs(entry: &OutputSlot<ColumnSource>, sources: &[(&str, &str)], context: &str) {
    let expected: Vec<(String, String)> = sources
        .iter()
        .map(|(table, column)| (table.to_string(), column.to_string()))
        .collect();
    assert_eq!(source_pairs(entry), expected, "sources for '{context}'");
}

fn assert_no_errors(result: &ModelColumnLineage) {
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
}

/// Assert that `column` was traced to exactly `sources` (in order) with the
/// given transformation.
fn assert_resolved(
    result: &ModelColumnLineage,
    column: &str,
    sources: &[(&str, &str)],
    transformation: TransformationType,
) {
    let traced = entry(result, column);
    let expected: Vec<(String, String)> = sources
        .iter()
        .map(|(table, column)| (table.to_string(), column.to_string()))
        .collect();
    assert_eq!(source_pairs(traced), expected, "sources for '{column}'");
    assert_eq!(
        traced.transformation, transformation,
        "transformation for '{column}'"
    );
}

/// Assert that `column` has no complete lineage and reported a column-scoped
/// error of the given kind whose message contains `message`. The column is
/// either absent or listed with a `Partial` value lineage. Returns the error
/// so the caller can inspect its hint.
fn assert_column_error<'a>(
    result: &'a ModelColumnLineage,
    column: &str,
    kind: ColumnLineageErrorKind,
    message: &str,
) -> &'a ColumnLineageError {
    assert!(
        result
            .slots()
            .iter()
            .filter(|entry| entry.column() == column)
            .all(|entry| entry.lineage.is_partial()),
        "column '{column}' was unexpectedly traced completely: {:?}",
        result.slots()
    );
    let error = result
        .errors
        .iter()
        .find(|error| error.column.as_deref() == Some(column))
        .unwrap_or_else(|| panic!("no error for '{column}': {:?}", result.errors));
    assert_eq!(error.kind, kind, "error kind for '{column}'");
    let prefix = format!("column '{column}': ");
    assert!(
        error.what.starts_with(&prefix),
        "message '{}' does not start with '{prefix}'",
        error.what
    );
    assert!(
        error.what.contains(message),
        "message '{}' does not contain '{message}' for column '{column}'",
        error.what
    );
    assert_eq!(error.why, None);
    error
}

/// Assert a model-level parse failure: the only error, with no output and
/// all counts zero.
fn assert_parse_failure(result: &ModelColumnLineage, why: &str) {
    assert_eq!(result.errors.len(), 1, "errors: {:?}", result.errors);
    let error = &result.errors[0];
    assert_eq!(error.kind, ColumnLineageErrorKind::ParseFailure);
    assert_eq!(error.column, None);
    assert_eq!(error.what, "failed to parse SQL for 'root'");
    let reason = error.why.as_deref().expect("a parse failure explains why");
    assert!(
        reason.contains(why),
        "reason '{reason}' does not contain '{why}'"
    );
    assert_eq!(error.hint.as_deref(), Some(PARSE_HINT));
    assert_eq!(result.output, None);
    assert_counts(result, 0, 0, 0);
}

mod basic;
mod discovery;
mod set_operations;
