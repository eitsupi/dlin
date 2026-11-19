//! Column lineage contracts for BigQuery SQL shapes seen in real projects.
//! The dialect comes from the manifest's `adapter_type`.

use super::*;
use std::fs;
use std::process::Command;

/// Write a BigQuery manifest whose models are `(name, compiled_code,
/// upstream model names)` and return its directory.
fn bigquery_manifest_dir(models: &[(&str, &str, &[&str])]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("target")).unwrap();
    let nodes: serde_json::Map<String, serde_json::Value> = models
        .iter()
        .map(|(name, sql, upstream)| {
            let unique_id = format!("model.test_project.{name}");
            let depends_on: Vec<String> = upstream
                .iter()
                .map(|model| format!("model.test_project.{model}"))
                .collect();
            let node = serde_json::json!({
                "unique_id": unique_id,
                "name": name,
                "resource_type": "model",
                "package_name": "test_project",
                "original_file_path": format!("models/{name}.sql"),
                "database": "p",
                "schema": "d",
                "alias": name,
                "relation_name": format!("`p`.`d`.`{name}`"),
                "depends_on": {"nodes": depends_on},
                "config": {"materialized": "view", "tags": []},
                "compiled_code": sql,
            });
            (unique_id, node)
        })
        .collect();
    let manifest = serde_json::json!({
        "metadata": {"adapter_type": "bigquery", "project_name": "test_project"},
        "nodes": nodes,
        "sources": {},
        "exposures": {},
    });
    fs::write(
        tmp.path().join("target/manifest.json"),
        serde_json::to_string(&manifest).unwrap(),
    )
    .unwrap();
    tmp
}

/// Run `dlin column upstream` as JSON and return the exit status and reports.
fn column_upstream_json(
    tmp: &tempfile::TempDir,
    models: &[&str],
) -> (std::process::ExitStatus, serde_json::Value) {
    let manifest_path = tmp.path().join("target/manifest.json");
    let output = Command::new(binary_path())
        .args(["column", "upstream"])
        .args(models)
        .args(["-o", "json", "--no-cache", "--manifest-path"])
        .arg(&manifest_path)
        .current_dir(tmp.path())
        .output()
        .expect("failed to run dlin");
    let reports = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout should be JSON ({error}); stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status, reports)
}

/// The lineage of the named output column of the named model's report.
fn column_lineage<'a>(
    reports: &'a serde_json::Value,
    model: &str,
    column: &str,
) -> &'a serde_json::Value {
    let report = reports
        .as_array()
        .unwrap()
        .iter()
        .find(|report| report["model"].as_str() == Some(model))
        .unwrap_or_else(|| panic!("no report for {model}: {reports}"));
    let slot = report["output"]["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|slot| slot["name"]["ident"]["value"].as_str() == Some(column))
        .unwrap_or_else(|| panic!("no column {column} in {model}: {report}"));
    &slot["lineage"]
}

/// `table.column` for each known source, sorted.
fn known_sources(lineage: &serde_json::Value) -> Vec<String> {
    let mut known: Vec<String> = lineage["known"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| {
            format!(
                "{}.{}",
                source["table"].as_str().unwrap(),
                source["column"]["value"].as_str().unwrap()
            )
        })
        .collect();
    known.sort();
    known
}

#[test]
fn normalization_mode_is_not_a_column() {
    let tmp = bigquery_manifest_dir(&[
        (
            "upstream_model",
            r#"select id, name_col from `p`.`d`.`external_table_a`"#,
            &[],
        ),
        (
            "norm_external",
            r#"select
  normalize(name_col, nfkc) as norm_lower
  , normalize(name_col, NFKC) as norm_upper
  , normalize_and_casefold(name_col, nfd) as folded
from `p`.`d`.`external_table_a`"#,
            &[],
        ),
        (
            "norm_upstream",
            r#"select
  normalize(name_col, nfkc) as norm_lower
  , normalize(name_col) as norm_default
from `p`.`d`.`upstream_model`"#,
            &["upstream_model"],
        ),
        (
            "norm_cte",
            r#"with base as (
  select name_col from `p`.`d`.`upstream_model`
)

select normalize(name_col, nfkc) as norm_lower
from base"#,
            &["upstream_model"],
        ),
    ]);
    let (status, reports) =
        column_upstream_json(&tmp, &["norm_external", "norm_upstream", "norm_cte"]);
    assert!(status.success(), "reports: {reports}");

    let expected = ["p.d.external_table_a.name_col".to_string()];
    for (model, column) in [
        ("norm_external", "norm_lower"),
        ("norm_external", "norm_upper"),
        ("norm_external", "folded"),
        ("norm_upstream", "norm_lower"),
        ("norm_upstream", "norm_default"),
        ("norm_cte", "norm_lower"),
    ] {
        let lineage = column_lineage(&reports, model, column);
        assert_eq!(lineage["status"], "complete", "{model}.{column}: {lineage}");
        assert_eq!(known_sources(lineage), expected, "{model}.{column}");
    }
    for report in reports.as_array().unwrap() {
        assert_eq!(report["errors"], serde_json::json!([]), "{report}");
    }
}

/// GA4-style lookups of an unaliased `UNNEST` over an array of STRUCTs, read
/// from a model and from an unmodelled table.
fn unaliased_unnest_dir() -> tempfile::TempDir {
    let lookup = |relation: &str| {
        format!(
            r#"select
  (select value.string_value from unnest(params) where key = "page") as page_bare
  , (select p.value.string_value from unnest(params) as p where p.key = "page") as page_aliased
from `p`.`d`.`{relation}`"#
        )
    };
    let upstream = lookup("upstream_model");
    let external = lookup("external_table_a");
    bigquery_manifest_dir(&[
        (
            "upstream_model",
            r#"select id, params from `p`.`d`.`external_table_a`"#,
            &[],
        ),
        ("param_upstream", &upstream, &["upstream_model"]),
        ("param_external", &external, &[]),
    ])
}

/// The candidates of the single `unverified` reason of a lineage, rendered
/// as `relation` or `element of relation.column`.
fn unverified_candidates(lineage: &serde_json::Value) -> (Vec<String>, Vec<String>) {
    let unresolved = lineage["unresolved"].as_array().unwrap();
    assert_eq!(unresolved.len(), 1, "{lineage}");
    let reason = &unresolved[0]["reason"];
    assert_eq!(reason["kind"], "unverified", "{lineage}");
    assert_eq!(reason["column"]["value"], "value");
    let render = |candidates: &serde_json::Value| -> Vec<String> {
        candidates
            .as_array()
            .unwrap()
            .iter()
            .map(|candidate| match candidate["kind"].as_str().unwrap() {
                "relation" => candidate["relation"]["name"]["value"]
                    .as_str()
                    .unwrap()
                    .to_string(),
                "array_element" => candidate["array_sources"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|source| {
                        format!(
                            "element of {}.{}",
                            source["relation"]["name"]["value"].as_str().unwrap(),
                            source["column"]["value"].as_str().unwrap()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
                other => panic!("unexpected candidate kind {other}"),
            })
            .collect()
    };
    (
        render(&reason["present_candidates"]),
        render(&reason["possible_candidates"]),
    )
}

#[test]
fn unaliased_unnest_field_is_not_a_column_of_the_outer_relation() {
    let tmp = unaliased_unnest_dir();
    let (status, reports) = column_upstream_json(&tmp, &["param_upstream", "param_external"]);
    assert!(
        status.success(),
        "an unverified name is not fatal: {reports}"
    );

    // The upstream model lists no `value`, so only the element can carry it.
    let bare = column_lineage(&reports, "param_upstream", "page_bare");
    assert_eq!(bare["status"], "partial");
    assert_eq!(known_sources(bare), Vec::<String>::new());
    assert_eq!(
        unverified_candidates(bare),
        (vec![], vec!["element of upstream_model.params".to_string()])
    );

    // The columns of the unmodelled table are not known, so both may carry
    // it, and neither becomes a known source.
    let bare = column_lineage(&reports, "param_external", "page_bare");
    assert_eq!(bare["status"], "partial");
    assert_eq!(known_sources(bare), Vec::<String>::new());
    assert_eq!(
        unverified_candidates(bare),
        (
            vec![],
            vec![
                "external_table_a".to_string(),
                "element of external_table_a.params".to_string()
            ]
        )
    );

    for model in ["param_upstream", "param_external"] {
        let aliased = column_lineage(&reports, model, "page_aliased");
        assert_eq!(aliased["status"], "complete", "{model}: {aliased}");
        assert_eq!(
            known_sources(aliased),
            ["p.d.external_table_a.params".to_string()],
            "{model}"
        );
        let report = reports
            .as_array()
            .unwrap()
            .iter()
            .find(|report| report["model"] == model)
            .unwrap();
        let errors = report["errors"].as_array().unwrap();
        assert_eq!(errors.len(), 1, "{report}");
        assert_eq!(errors[0]["kind"], "column_indeterminate");
        assert_eq!(errors[0]["column"], "page_bare");
    }
}

#[test]
fn unaliased_unnest_field_is_a_possible_downstream_impact_of_the_array() {
    let tmp = unaliased_unnest_dir();
    let output = Command::new(binary_path())
        .args([
            "column",
            "downstream",
            "upstream_model",
            "--column",
            "params",
            "-o",
            "json",
            "--no-cache",
            "--manifest-path",
        ])
        .arg(tmp.path().join("target/manifest.json"))
        .current_dir(tmp.path())
        .output()
        .expect("failed to run dlin");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reports: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let targets = |key: &str| -> Vec<String> {
        reports[0][key]
            .as_array()
            .unwrap()
            .iter()
            .map(|impact| {
                format!(
                    "{}.{}",
                    impact["target"]["model"].as_str().unwrap(),
                    impact["target"]["column"].as_str().unwrap()
                )
            })
            .collect()
    };
    assert_eq!(targets("confirmed"), ["param_upstream.page_aliased"]);
    assert_eq!(targets("possible"), ["param_upstream.page_bare"]);
}
