use dlin_sqllineage::{Analyzer, DiagnosticKind, Dialect, Severity};

#[test]
fn join_on_subquery_keeps_empty_projection_warning() {
    let result = Analyzer::new(Dialect::Generic)
        .analyze("SELECT a.id FROM a JOIN b ON EXISTS (SELECT FROM c)")
        .unwrap()
        .into_single()
        .unwrap();
    assert_eq!(
        result
            .read
            .iter()
            .map(|relation| relation.name.value.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(result.output.columns.len(), 1);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Warning
                && diagnostic.kind == DiagnosticKind::EmptyProjection)
    );
}

#[test]
fn nested_empty_projections_in_other_clauses_preserve_analysis() {
    let cases = [
        "SELECT a.id FROM a ORDER BY (SELECT FROM c)",
        "SELECT a.id FROM a LIMIT (SELECT FROM c)",
        "SELECT a.id FROM a OFFSET (SELECT FROM c)",
        "SELECT a.id FROM a GROUP BY (SELECT FROM c)",
        "SELECT a.id FROM a HAVING EXISTS (SELECT FROM c)",
        "SELECT a.id FROM a QUALIFY EXISTS (SELECT FROM c)",
        "SELECT a.id FROM a WINDOW w AS (ORDER BY (SELECT FROM c))",
        "SELECT SUM(id) FILTER (WHERE EXISTS (SELECT FROM c)) FROM a",
        "SELECT ARRAY_AGG(id ORDER BY (SELECT FROM c)) FROM a",
        "SELECT SUM(id) OVER (ORDER BY (SELECT FROM c)) FROM a",
        "SELECT COALESCE((SELECT FROM c), 1) FROM a",
        "SELECT a.id FROM a WHERE COALESCE((SELECT FROM c), 1) = 1",
        "SELECT a.id FROM a WHERE TRUE IS DISTINCT FROM EXISTS (SELECT FROM c)",
        "VALUES ((SELECT FROM c))",
        "INSERT INTO a (id) VALUES ((SELECT FROM c))",
        "INSERT INTO a (id) VALUES (1) RETURNING (SELECT FROM c)",
        "INSERT INTO a (id) VALUES (1) ON CONFLICT (id) DO UPDATE SET id = (SELECT FROM c)",
        "UPDATE a SET id = (SELECT FROM c)",
        "UPDATE a SET id = 1 RETURNING (SELECT FROM c)",
        "UPDATE a SET id = 1 WHERE EXISTS (SELECT FROM c)",
        "DELETE FROM a WHERE EXISTS (SELECT FROM c)",
        "DELETE FROM a RETURNING (SELECT FROM c)",
        "MERGE INTO a USING b ON EXISTS (SELECT FROM c) WHEN MATCHED THEN DELETE",
        "MERGE INTO a USING b ON a.id = b.id WHEN MATCHED AND EXISTS (SELECT FROM c) THEN DELETE",
        "MERGE INTO a USING b ON a.id = b.id WHEN MATCHED THEN UPDATE SET id = (SELECT FROM c)",
        "EXPLAIN SELECT FROM c",
        "CREATE TABLE a (id INT CHECK (EXISTS (SELECT FROM c)))",
        "SELECT 1 FROM TABLE(f((SELECT FROM c)))",
        "SELECT 1 FROM f((SELECT FROM c)) AS x",
        "SELECT SUM(id) OVER (ROWS (SELECT FROM c) PRECEDING) FROM a",
        "UPDATE a JOIN b ON EXISTS (SELECT FROM c) SET id = 1",
        "DELETE FROM a JOIN b ON EXISTS (SELECT FROM c)",
        "SELECT a.id FROM a ORDER BY (SELECT 1 ORDER BY (SELECT FROM c))",
    ];
    for sql in cases {
        let analyze = |sql: &str| {
            Analyzer::new(Dialect::Generic)
                .analyze(sql)
                .unwrap_or_else(|error| panic!("{sql}: {error:?}"))
                .into_single()
                .unwrap()
        };
        let mut actual = analyze(sql);
        assert_eq!(
            actual
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.kind == DiagnosticKind::EmptyProjection)
                .count(),
            1,
            "{sql}: {:?}",
            actual.diagnostics
        );
        assert!(
            actual.diagnostics.iter().any(|diagnostic| diagnostic.kind
                == DiagnosticKind::EmptyProjection
                && diagnostic.severity == Severity::Warning),
            "{sql}"
        );
        actual
            .diagnostics
            .retain(|diagnostic| diagnostic.kind != DiagnosticKind::EmptyProjection);
        let control = analyze(&sql.replace("SELECT FROM c", "SELECT 1 FROM c"));
        assert_eq!(
            actual, control,
            "only the empty-projection diagnostic changes: {sql}"
        );
    }
}

#[test]
fn nested_empty_projection_keeps_dialect_severity_and_deduplicates() {
    for (dialect, severity) in [
        (Dialect::Generic, Severity::Warning),
        (Dialect::Snowflake, Severity::Error),
    ] {
        // Parse once with a dialect accepting empty projections, then retain
        // the analyzer dialect's diagnostic severity when reusing the AST.
        let result = dlin_sqllineage::parse(
            "SELECT a.id FROM a JOIN b ON EXISTS (SELECT FROM c) ORDER BY (SELECT FROM d)",
            Dialect::Generic,
        )
        .unwrap()
        .analyze(&Analyzer::new(dialect))
        .into_single()
        .unwrap();
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].kind, DiagnosticKind::EmptyProjection);
        assert_eq!(result.diagnostics[0].severity, severity);
        assert_eq!(
            result.output.columns.is_empty(),
            severity == Severity::Error
        );
        assert_eq!(
            result
                .read
                .iter()
                .map(|relation| relation.name.value.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
    }
}
