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
    run_column_upstream_json(tmp, models, &["--no-cache"])
}

fn run_column_upstream_json(
    tmp: &tempfile::TempDir,
    models: &[&str],
    args: &[&str],
) -> (std::process::ExitStatus, serde_json::Value) {
    let manifest_path = tmp.path().join("target/manifest.json");
    let output = Command::new(binary_path())
        .args(["column", "upstream"])
        .args(models)
        .args(args)
        .args(["-o", "json", "--manifest-path"])
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

#[test]
fn information_schema_views_keep_their_parts() {
    let tmp = bigquery_manifest_dir(&[(
        "jobs_model",
        r#"with a as (
  select job_id from `p`.`region-us.INFORMATION_SCHEMA`.`JOBS_BY_PROJECT`
)

, b as (
  select job_id from `p`.`region-us`.`INFORMATION_SCHEMA`.`JOBS_BY_PROJECT`
)

, c as (
  select job_id from `region-us`.INFORMATION_SCHEMA.JOBS_BY_PROJECT
)

select a.job_id as job_a, b.job_id as job_b, c.job_id as job_c
from a cross join b cross join c"#,
        &[],
    )]);
    let (status, reports) = column_upstream_json(&tmp, &["jobs_model"]);
    assert!(status.success(), "{reports}");
    for (column, table) in [
        ("job_a", "p.region-us.information_schema.JOBS_BY_PROJECT"),
        ("job_b", "p.region-us.information_schema.JOBS_BY_PROJECT"),
        ("job_c", "region-us.information_schema.jobs_by_project"),
    ] {
        let lineage = column_lineage(&reports, "jobs_model", column);
        assert_eq!(lineage["status"], "complete", "{column}: {lineage}");
        assert_eq!(
            known_sources(lineage),
            [format!("{table}.job_id")],
            "{column}"
        );
    }
    let boundaries = reports[0]["boundaries"].as_array().unwrap();
    assert_eq!(boundaries.len(), 3, "{boundaries:?}");
    for boundary in boundaries {
        let relation = &boundary["resource"]["relation"];
        assert_eq!(relation["namespace"], "information_schema", "{relation}");
        assert_eq!(relation["schema"]["value"], "region-us", "{relation}");
        assert!(
            !boundary["resource"]["name"]
                .as_str()
                .unwrap()
                .contains(['"', '`']),
            "{boundary}"
        );
    }
}

/// A source declared on a region view, the way dbt sources name
/// `INFORMATION_SCHEMA`: the schema holds the region and the namespace, and
/// is rendered as one quoted part.
#[test]
fn information_schema_source_matches_its_sql_references() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("target")).unwrap();
    let manifest = serde_json::json!({
        "metadata": {"adapter_type": "bigquery", "project_name": "test_project"},
        "nodes": {
            "model.test_project.jobs_model": {
                "unique_id": "model.test_project.jobs_model",
                "name": "jobs_model",
                "resource_type": "model",
                "package_name": "test_project",
                "original_file_path": "models/jobs_model.sql",
                "database": "p",
                "schema": "d",
                "alias": "jobs_model",
                "relation_name": "`p`.`d`.`jobs_model`",
                "depends_on": {"nodes": ["source.test_project.info.JOBS_BY_PROJECT"]},
                "config": {"materialized": "view", "tags": []},
                "compiled_code": r#"with quoted_together as (
  select * from `p`.`region-us.INFORMATION_SCHEMA`.`JOBS_BY_PROJECT`
)

, quoted_apart as (
  select job_id from `p`.`region-us`.`INFORMATION_SCHEMA`.`JOBS_BY_PROJECT`
)

select quoted_together.user_email, quoted_apart.job_id
from quoted_together cross join quoted_apart"#,
            }
        },
        "sources": {
            "source.test_project.info.JOBS_BY_PROJECT": {
                "unique_id": "source.test_project.info.JOBS_BY_PROJECT",
                "name": "JOBS_BY_PROJECT",
                "source_name": "info",
                "resource_type": "source",
                "package_name": "test_project",
                "database": "p",
                "schema": "region-us.INFORMATION_SCHEMA",
                "identifier": "JOBS_BY_PROJECT",
                "relation_name": "`p`.`region-us.INFORMATION_SCHEMA`.`JOBS_BY_PROJECT`",
                "columns": {
                    "job_id": {"name": "job_id"},
                    "user_email": {"name": "user_email"}
                }
            }
        },
        "exposures": {},
    });
    fs::write(
        tmp.path().join("target/manifest.json"),
        serde_json::to_string(&manifest).unwrap(),
    )
    .unwrap();

    let (status, reports) = column_upstream_json(&tmp, &["jobs_model"]);
    assert!(status.success(), "{reports}");
    // The star expands to the source's declared columns, so the source is
    // what the SQL reads.
    for column in ["user_email", "job_id"] {
        let lineage = column_lineage(&reports, "jobs_model", column);
        assert_eq!(lineage["status"], "complete", "{column}: {lineage}");
        assert_eq!(
            known_sources(lineage),
            [format!(
                "p.region-us.information_schema.JOBS_BY_PROJECT.{column}"
            )],
            "{column}"
        );
    }
    let boundaries = reports[0]["boundaries"].as_array().unwrap();
    assert!(
        boundaries
            .iter()
            .all(|boundary| boundary["resource"]["unique_id"]
                == "source.test_project.info.JOBS_BY_PROJECT"),
        "{boundaries:?}"
    );
}

/// A model joined with a seed whose columns are not declared, as in a
/// real-project report: the seed may or may not have a column of the same
/// name, so the name is left to its columns instead of failing the run.
fn seed_join_dir(seed_columns: &[&str]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("target")).unwrap();
    let columns: serde_json::Map<String, serde_json::Value> = seed_columns
        .iter()
        .map(|name| ((*name).to_string(), serde_json::json!({"name": name})))
        .collect();
    let model = |name: &str, sql: &str, upstream: &[&str], columns: &[&str]| {
        serde_json::json!({
            "unique_id": format!("model.test_project.{name}"),
            "name": name,
            "resource_type": "model",
            "package_name": "test_project",
            "original_file_path": format!("models/{name}.sql"),
            "database": "p",
            "schema": "d",
            "alias": name,
            "relation_name": format!("`p`.`d`.`{name}`"),
            "depends_on": {"nodes": upstream},
            "config": {"materialized": "view", "tags": []},
            "columns": columns
                .iter()
                .map(|column| ((*column).to_string(), serde_json::json!({"name": column})))
                .collect::<serde_json::Map<_, _>>(),
            "compiled_code": sql,
        })
    };
    let manifest = serde_json::json!({
        "metadata": {"adapter_type": "bigquery", "project_name": "test_project"},
        "nodes": {
            "seed.test_project.category_seed": {
                "unique_id": "seed.test_project.category_seed",
                "name": "category_seed",
                "resource_type": "seed",
                "package_name": "test_project",
                "original_file_path": "seeds/category_seed.csv",
                "database": "p",
                "schema": "d",
                "alias": "category_seed",
                "relation_name": "`p`.`d`.`category_seed`",
                "depends_on": {"nodes": []},
                "config": {"materialized": "seed", "tags": []},
                "columns": columns,
            },
            "model.test_project.events_model": model(
                "events_model",
                r#"select event_source, event_medium from `p`.`d`.`external_table_a`"#,
                &[],
                &["event_source", "event_medium"],
            ),
            "model.test_project.medium_model": model(
                "medium_model",
                r#"select source, event_medium from `p`.`d`.`external_table_b`"#,
                &[],
                &["source", "event_medium"],
            ),
            "model.test_project.join_model": model(
                "join_model",
                r#"select
  e.event_source
  , event_medium
  , source_category
from `p`.`d`.`events_model` as e
  left join `p`.`d`.`category_seed` as s on e.event_source = s.source"#,
                &["model.test_project.events_model", "seed.test_project.category_seed"],
                &["event_source", "event_medium", "source_category"],
            ),
            "model.test_project.two_models": model(
                "two_models",
                r#"select event_medium
from `p`.`d`.`events_model` as e
  join `p`.`d`.`medium_model` as m on e.event_source = m.source"#,
                &["model.test_project.events_model", "model.test_project.medium_model"],
                &["event_medium"],
            ),
        },
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

#[test]
fn a_seed_without_declared_columns_leaves_a_name_unverified() {
    let tmp = seed_join_dir(&[]);
    let (status, reports) = column_upstream_json(&tmp, &["join_model"]);
    assert!(
        status.success(),
        "an unverified name is not fatal: {reports}"
    );
    // `events_model` publishes `event_medium`; the seed may too, so neither
    // is chosen.
    let medium = column_lineage(&reports, "join_model", "event_medium");
    assert_eq!(medium["status"], "partial", "{medium}");
    assert_eq!(known_sources(medium), Vec::<String>::new());
    let reason = &medium["unresolved"][0]["reason"];
    assert_eq!(reason["kind"], "unverified", "{medium}");
    let names = |key: &str| -> Vec<String> {
        reason[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|candidate| {
                candidate["relation"]["name"]["value"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    };
    assert_eq!(names("present_candidates"), ["events_model"]);
    assert_eq!(names("possible_candidates"), ["category_seed"]);
    // Only the seed can have `source_category`.
    let category = column_lineage(&reports, "join_model", "source_category");
    assert_eq!(category["status"], "complete", "{category}");
    let errors = reports[0]["errors"].as_array().unwrap();
    assert!(
        errors
            .iter()
            .all(|error| error["kind"] == "column_indeterminate"),
        "{errors:?}"
    );

    // Declaring the seed's columns settles the name.
    let tmp = seed_join_dir(&["source", "source_category"]);
    let (status, reports) = column_upstream_json(&tmp, &["join_model"]);
    assert!(status.success(), "{reports}");
    let medium = column_lineage(&reports, "join_model", "event_medium");
    assert_eq!(medium["status"], "complete", "{medium}");
}

#[test]
fn two_models_known_to_publish_a_name_stay_ambiguous() {
    let tmp = seed_join_dir(&[]);
    let (status, reports) = column_upstream_json(&tmp, &["two_models"]);
    assert!(!status.success(), "a known conflict is fatal: {reports}");
    let medium = column_lineage(&reports, "two_models", "event_medium");
    assert_eq!(
        medium["unresolved"][0]["reason"]["kind"], "ambiguous",
        "{medium}"
    );
    assert_eq!(reports[0]["errors"][0]["kind"], "column_ambiguous");
}

#[test]
fn seed_csv_settles_a_join_and_header_changes_refresh_cached_lineage() {
    let tmp = seed_join_dir(&[]);
    fs::write(tmp.path().join("dbt_project.yml"), "name: test_project\n").unwrap();
    fs::create_dir(tmp.path().join("seeds")).unwrap();
    let csv = tmp.path().join("seeds/category_seed.csv");
    fs::write(&csv, "source,source_category\nweb,referral\n").unwrap();

    let (status, reports) = run_column_upstream_json(&tmp, &["join_model"], &[]);
    assert!(status.success(), "{reports}");
    let medium = column_lineage(&reports, "join_model", "event_medium");
    assert_eq!(medium["status"], "complete", "{medium}");
    assert_eq!(known_sources(medium), ["p.d.external_table_a.event_medium"]);
    let category = column_lineage(&reports, "join_model", "source_category");
    assert_eq!(
        known_sources(category),
        ["p.d.category_seed.source_category"]
    );
    assert!(
        reports[0]["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| {
                record["origin"] == "seed_file"
                    && record["subject"] == "seed.test_project.category_seed"
                    && record["coverage"] == "exhaustive"
                    && record["order"] == "proven"
            }),
        "{reports}"
    );

    fs::write(
        &csv,
        "source,source_category,event_medium\nweb,referral,email\n",
    )
    .unwrap();
    let (status, reports) = run_column_upstream_json(&tmp, &["join_model"], &[]);
    assert!(
        !status.success(),
        "the changed header creates an ambiguity: {reports}"
    );
    let medium = column_lineage(&reports, "join_model", "event_medium");
    assert_eq!(
        medium["unresolved"][0]["reason"]["kind"], "ambiguous",
        "{medium}"
    );

    fs::remove_file(&csv).unwrap();
    let (status, reports) = run_column_upstream_json(&tmp, &["join_model"], &[]);
    assert!(status.success(), "{reports}");
    let medium = column_lineage(&reports, "join_model", "event_medium");
    assert_eq!(
        medium["unresolved"][0]["reason"]["kind"], "unverified",
        "{medium}"
    );
}

/// A STRUCT built in a CTE and expanded with `item.*` next to a column of
/// its own: the column is defined once, so it is not ambiguous, and each
/// field keeps the column it was built from.
#[test]
fn struct_field_star_next_to_a_listed_column() {
    let tmp = bigquery_manifest_dir(&[
        (
            "constant_struct",
            r#"with packed as (select struct(1 as col_a) as item), expanded as (select item.*, 2 as flag from packed) select flag from expanded"#,
            &[],
        ),
        (
            "external_struct",
            r#"with packed as (
  select struct(id as col_a, name_col as col_b) as item
  from `p`.`d`.`external_table_a`
), expanded as (
  select item.*, 2 as flag from packed
)
select col_a, col_b, flag from expanded"#,
            &[],
        ),
    ]);
    let (status, reports) = column_upstream_json(&tmp, &["constant_struct", "external_struct"]);
    assert!(status.success(), "reports: {reports}");

    for (model, column, expected) in [
        ("constant_struct", "flag", vec![]),
        ("external_struct", "flag", vec![]),
        (
            "external_struct",
            "col_a",
            vec!["p.d.external_table_a.id".to_string()],
        ),
        (
            "external_struct",
            "col_b",
            vec!["p.d.external_table_a.name_col".to_string()],
        ),
    ] {
        let lineage = column_lineage(&reports, model, column);
        assert_eq!(lineage["status"], "complete", "{model}.{column}: {lineage}");
        assert_eq!(known_sources(lineage), expected, "{model}.{column}");
    }
    for report in reports.as_array().unwrap() {
        assert_eq!(report["errors"], serde_json::json!([]), "{report}");
    }
}

#[test]
fn unknown_field_star_overlap_is_nonfatal_across_models() {
    let tmp = bigquery_manifest_dir(&[
        (
            "uncertain",
            r#"with expanded as (select s.item.*, 2 as flag from src s) select flag from expanded"#,
            &[],
        ),
        (
            "reader",
            r#"select flag as renamed from `p`.`d`.`uncertain`"#,
            &["uncertain"],
        ),
        (
            "duplicate",
            r#"with expanded as (select s.item.*, 2 as flag, 3 as flag from src s) select flag from expanded"#,
            &[],
        ),
    ]);
    let (status, reports) = column_upstream_json(&tmp, &["uncertain", "reader"]);
    assert_eq!(status.code(), Some(0), "{reports}");
    for (model, column) in [("uncertain", "flag"), ("reader", "renamed")] {
        let lineage = column_lineage(&reports, model, column);
        assert_eq!(lineage["status"], "partial", "{lineage}");
        assert_eq!(
            lineage["unresolved"][0]["reason"]["kind"], "unverified",
            "{lineage}"
        );
    }
    for report in reports.as_array().unwrap() {
        assert!(
            report["errors"].as_array().unwrap().iter().any(|error| {
                error["kind"] == "column_indeterminate"
                    && error["what"].as_str().unwrap().contains("unexpanded star")
            }),
            "{report}"
        );
    }
    let (status, reports) = column_upstream_json(&tmp, &["duplicate"]);
    assert_eq!(status.code(), Some(1), "{reports}");
    assert_eq!(
        reports[0]["errors"][0]["kind"], "column_ambiguous",
        "{reports}"
    );
}

#[test]
fn downstream_preserves_the_requested_columns_binding_diagnostic() {
    let tmp = bigquery_manifest_dir(&[
        (
            "uncertain",
            r#"with expanded as (select s.item.*, 2 as flag from src s) select flag from expanded"#,
            &[],
        ),
        (
            "duplicate",
            r#"with expanded as (select s.item.*, 2 as flag, 3 as flag from src s) select flag from expanded"#,
            &[],
        ),
    ]);
    for (model, code, kind) in [
        ("uncertain", 0, "column_indeterminate"),
        ("duplicate", 1, "column_ambiguous"),
    ] {
        let output = Command::new(binary_path())
            .args([
                "column",
                "downstream",
                model,
                "--column",
                "flag",
                "-o",
                "json",
                "--no-cache",
                "--manifest-path",
            ])
            .arg(tmp.path().join("target/manifest.json"))
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let reports: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(code), "{reports}");
        assert_eq!(reports[0]["errors"][0]["kind"], kind, "{reports}");
        assert_eq!(
            reports[0]["exploration"][0]["status"], "incomplete",
            "{reports}"
        );
    }
}
