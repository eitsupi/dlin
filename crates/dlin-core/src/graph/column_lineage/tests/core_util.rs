use super::*;

#[test]
fn test_format_lineage_error_strips_position() {
    let err = polyglot_sql::Error::parse("Cannot find column 'x' in query", 0, 0, 0, 0);
    let formatted = format_lineage_error(&err);
    assert_eq!(formatted, "lineage failed: Cannot find column 'x' in query");
    assert!(
        !formatted.contains("line 0"),
        "should strip meaningless position info"
    );
}

#[test]
fn test_format_lineage_error_preserves_real_position() {
    let err = polyglot_sql::Error::parse("unexpected token", 5, 10, 0, 0);
    let formatted = format_lineage_error(&err);
    assert!(
        formatted.contains("line 5"),
        "should preserve real position info: {}",
        formatted
    );
}

#[test]
fn test_format_lineage_error_internal() {
    let err = polyglot_sql::Error::internal("lineage recursion depth exceeded");
    let formatted = format_lineage_error(&err);
    assert_eq!(
        formatted,
        "lineage failed: lineage recursion depth exceeded"
    );
}

#[test]
fn test_column_resolution_reasons_are_handled_structurally() {
    let target = polyglot_sql::ColumnResolutionTarget::Name {
        name: "wanted".to_string(),
    };

    let not_found = polyglot_sql::Error::column_resolution(
        target.clone(),
        polyglot_sql::ColumnResolutionReason::NotFound,
    );
    let indeterminate = polyglot_sql::Error::column_resolution(
        target.clone(),
        polyglot_sql::ColumnResolutionReason::Indeterminate,
    );
    let ambiguous = polyglot_sql::Error::column_resolution(
        target,
        polyglot_sql::ColumnResolutionReason::Ambiguous,
    );

    assert!(!super::super::is_indeterminate_column_resolution(
        &not_found
    ));
    assert!(super::super::is_indeterminate_column_resolution(
        &indeterminate
    ));
    assert!(!super::super::is_indeterminate_column_resolution(
        &ambiguous
    ));

    // The structured reason is internal; dlin's existing error text remains
    // stable for all name-resolution failures.
    for error in [&not_found, &indeterminate, &ambiguous] {
        assert_eq!(
            format_lineage_error(error),
            "Cannot find column 'wanted' in query"
        );
    }
}

#[test]
fn test_lineage_at_traces_an_unaliased_expression_by_ordinal() {
    let expression =
        polyglot_sql::parse_one("SELECT fee * 2 FROM t", polyglot_sql::DialectType::Generic)
            .unwrap();
    let node = polyglot_sql::lineage::lineage_at(0, &expression, None, false).unwrap();

    assert!(
        node.walk().any(|node| node.name == "t.fee"),
        "ordinal lineage should reach t.fee: {:?}",
        node
    );
}

#[test]
fn test_output_columns_preserves_duplicate_named_ordinals() {
    let expression = polyglot_sql::parse_one(
        "SELECT a.id, b.id FROM a JOIN b ON a.id = b.id",
        polyglot_sql::DialectType::Generic,
    )
    .unwrap();
    let output = polyglot_sql::lineage::output_columns(&expression, None).unwrap();

    assert_eq!(
        output.columns,
        vec![
            polyglot_sql::OutputColumn::Named {
                name: "id".to_string(),
                ordinal: Some(0),
            },
            polyglot_sql::OutputColumn::Named {
                name: "id".to_string(),
                ordinal: Some(1),
            },
        ]
    );
}

#[test]
fn test_set_branch_ordinals_survive_an_unresolved_sibling() {
    let left_survives = polyglot_sql::parse_one(
        "SELECT id, amt FROM a EXCEPT SELECT * FROM unknown",
        polyglot_sql::DialectType::Generic,
    )
    .unwrap();
    let node = polyglot_sql::lineage::lineage_at(0, &left_survives, None, false).unwrap();
    assert_eq!(node.downstream.len(), 1);
    assert_eq!(
        node.downstream[0].set_branch.map(|branch| branch.ordinal),
        Some(0)
    );

    let right_survives = polyglot_sql::parse_one(
        "SELECT * FROM unknown EXCEPT SELECT id, amt FROM a",
        polyglot_sql::DialectType::Generic,
    )
    .unwrap();
    let node = polyglot_sql::lineage::lineage_at(0, &right_survives, None, false).unwrap();
    assert_eq!(node.downstream.len(), 1);
    assert_eq!(
        node.downstream[0].set_branch.map(|branch| branch.ordinal),
        Some(1)
    );
}
