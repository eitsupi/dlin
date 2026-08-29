use super::*;

#[test]
fn test_column_impact_excludes_off_path_errors() {
    let manifest = make_off_path_error_manifest();
    let result = compute_column_impact(
        &manifest,
        "source_model",
        "col_x",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    // relevant_model.col_x must be on the impact path
    assert!(
        result
            .impacted_columns
            .iter()
            .any(|ic| ic.model == "relevant_model" && ic.column == "col_x"),
        "relevant_model.col_x should be impacted, got: {:?}",
        result.impacted_columns
    );

    // sibling_model is not on the path for col_x
    assert!(
        !result
            .impacted_columns
            .iter()
            .any(|ic| ic.model == "sibling_model"),
        "sibling_model should not be impacted, got: {:?}",
        result.impacted_columns
    );

    // sibling_fail error (from off-path sibling_model) must not appear
    let sibling_errors: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.what.contains("sibling_fail"))
        .collect();
    assert!(
        sibling_errors.is_empty(),
        "off-path errors from sibling_model should not appear in the impact report, \
         got errors: {:?}",
        result.errors
    );
}

/// Downstream model with no compiled SQL: its NoCompiledCode error must appear in the
/// impact report even though `found_on_path` is false (model-level failures are always
/// propagated so users know the analysis is incomplete).
#[test]
fn test_column_impact_propagates_model_level_errors_from_unreachable_downstream() {
    let mut manifest = make_off_path_error_manifest();

    // Add a model that depends on source_model but has no compiled_code.
    // It references col_x (same column we track), but we can never confirm this
    // because the model can't be analyzed.
    let mut cols = std::collections::HashMap::new();
    cols.insert(
        "col_x".to_string(),
        crate::parser::manifest::ManifestColumn {
            name: "col_x".to_string(),
        },
    );
    manifest.nodes.insert(
        "model.proj.broken_downstream".to_string(),
        crate::parser::manifest::ManifestNode {
            unique_id: "model.proj.broken_downstream".to_string(),
            name: "broken_downstream".to_string(),
            alias: None,
            resource_type: "model".to_string(),
            depends_on: crate::parser::manifest::DependsOn {
                nodes: vec!["model.proj.source_model".to_string()],
            },
            config: crate::parser::manifest::ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: cols,
            compiled_code: None, // no compiled SQL
            database: None,
            schema: None,
        },
    );

    let result = compute_column_impact(
        &manifest,
        "source_model",
        "col_x",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    // broken_downstream can't be analyzed, so its NoCompiledCode error must appear
    let has_broken_error = result
        .errors
        .iter()
        .any(|e| e.what.contains("broken_downstream"));
    assert!(
        has_broken_error,
        "model-level errors from unanalyzable downstream models should appear in the report, \
         got errors: {:?}",
        result.errors
    );
    // But sibling_fail (ColumnNotFound from off-path sibling_model) must still be absent
    let has_sibling_error = result
        .errors
        .iter()
        .any(|e| e.what.contains("sibling_fail"));
    assert!(
        !has_sibling_error,
        "ColumnNotFound from off-path model must not appear, got errors: {:?}",
        result.errors
    );
}

// --- ColumnLineageCache tests ---
