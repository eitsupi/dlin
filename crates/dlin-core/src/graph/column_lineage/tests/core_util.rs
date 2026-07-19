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
