use std::borrow::Cow;

use dlin_sqllineage::*;

fn one(sql: &str) -> StatementAnalysis {
    Analyzer::new(Dialect::Generic)
        .analyze(sql)
        .unwrap()
        .into_single()
        .unwrap()
}

#[test]
fn analyzer_is_send_sync_and_reusable() {
    fn check<T: Send + Sync>() {}
    check::<Analyzer<'_>>();
    let analyzer = Analyzer::new(Dialect::Generic);
    assert_eq!(
        analyzer.analyze("SELECT x FROM t").unwrap(),
        analyzer.analyze("SELECT x FROM t").unwrap()
    );
}

#[test]
fn parsing_reuse_debug_and_json_are_always_available() {
    let parsed = parse("SELECT x FROM t; SELECT 1", Dialect::Generic).unwrap();
    assert_eq!(parsed.statement_count(), 2);
    assert!(parsed.ast_debug().contains("Query"));
    let json: serde_json::Value = serde_json::from_str(&parsed.ast_json(false).unwrap()).unwrap();
    assert_eq!(json.as_array().unwrap().len(), 2);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&parsed.ast_json(true).unwrap()).unwrap(),
        json
    );
    assert_eq!(
        parsed.analyze(&Analyzer::new(Dialect::Generic)),
        Analyzer::new(Dialect::Generic)
            .analyze("SELECT x FROM t; SELECT 1")
            .unwrap()
    );
}

#[test]
fn single_statement_errors_are_structured() {
    assert_eq!(
        Analyzer::new(Dialect::Generic)
            .analyze("")
            .unwrap()
            .into_single(),
        Err(SingleStatementError::NoStatements)
    );
    assert_eq!(
        Analyzer::new(Dialect::Generic)
            .analyze("SELECT 1; SELECT 2")
            .unwrap()
            .into_single(),
        Err(SingleStatementError::Multiple { count: 2 })
    );
}

#[test]
fn parse_error_kinds_and_recursion_limit() {
    let analyzer = Analyzer::new(Dialect::Generic);
    assert_eq!(
        analyzer.analyze("SELECT (").unwrap_err().kind,
        ParseErrorKind::Syntax
    );
    assert_eq!(
        analyzer.analyze("SELECT 'unterminated").unwrap_err().kind,
        ParseErrorKind::Tokenizer
    );
    let error = analyzer
        .with_options(Options {
            max_recursion_depth: Some(1),
        })
        .analyze("SELECT (((a))) FROM t")
        .unwrap_err();
    assert_eq!(error.kind, ParseErrorKind::RecursionLimit);
}

#[test]
fn semantic_error_keeps_other_statements() {
    let results = Analyzer::new(Dialect::Generic)
        .analyze("SELECT x FROM t; SELECT x FROM t UNION ALL SELECT x, y FROM u; SELECT z FROM v")
        .unwrap();
    assert_eq!(results.statements.len(), 3);
    let bad = &results.statements[1];
    assert!(bad.output.columns.is_empty());
    assert!(bad.diagnostics.iter().any(|d| d.severity == Severity::Error
        && d.kind == DiagnosticKind::SetOperationArityMismatch { left: 1, right: 2 }));
    assert_eq!(results.statements[0].output.columns.len(), 1);
    assert_eq!(results.statements[2].output.columns.len(), 1);
}

#[test]
fn unsupported_syntax_is_a_statement_error() {
    let results = Analyzer::new(Dialect::Generic)
        .analyze("SELECT * FROM t |> SELECT x; SELECT y FROM u")
        .unwrap();
    assert!(results.statements[0].output.columns.is_empty());
    assert!(matches!(
        results.statements[0].diagnostics[0].kind,
        DiagnosticKind::UnsupportedSyntax {
            what: "pipe operators"
        }
    ));
    assert_eq!(results.statements[1].output.columns.len(), 1);
}

#[test]
fn output_names_distinguish_explicit_inferred_and_anonymous() {
    let output = one("SELECT x, SUM(y), 1 + 2, 3 AS \"?column?\" FROM t").output;
    assert!(matches!(&output.columns[0].name, OutputName::Explicit(id) if id.value == "x"));
    assert!(matches!(&output.columns[1].name, OutputName::Inferred(id) if id.value == "SUM"));
    assert_eq!(output.columns[2].name, OutputName::Anonymous);
    assert!(
        matches!(&output.columns[3].name, OutputName::Explicit(id) if id.value == "?column?" && id.quote == Some(Quote::Double))
    );
    assert_eq!(
        output.columns.iter().map(|c| c.ordinal).collect::<Vec<_>>(),
        vec![Some(0), Some(1), Some(2), Some(3)]
    );
}

#[test]
fn duplicates_and_quoted_star_are_real_output_columns() {
    let output = one("SELECT x AS same, y AS same, 1 AS \"*\" FROM t").output;
    assert!(
        matches!(output.lookup("same", Dialect::Generic), Lookup::Duplicate(cols) if cols.len() == 2)
    );
    assert_eq!(output.columns.len(), 3);
    assert_eq!(output.shape, Shape::Closed);
    assert_eq!(
        output.columns[2].name,
        OutputName::Explicit(Ident::quoted("*", Quote::Double))
    );
}

#[test]
fn open_output_keeps_fixed_prefix_and_unknown_suffix_ordinals() {
    let output = one("SELECT a, t.*, b FROM t").output;
    assert_eq!(output.columns.len(), 2);
    assert_eq!(output.columns[0].ordinal, Some(0));
    assert_eq!(output.columns[1].ordinal, None);
    assert!(
        matches!(&output.shape, Shape::Open { unexpanded } if unexpanded.len() == 1 && unexpanded[0].after_ordinal == Some(0) && unexpanded[0].relation.as_ref().unwrap().name.value == "t")
    );
    assert_eq!(
        output.lookup("missing", Dialect::Generic),
        Lookup::Absent {
            may_hide_behind_star: true
        }
    );
}

#[test]
fn expression_star_has_no_fabricated_relation() {
    let result = Analyzer::new(Dialect::BigQuery)
        .analyze("SELECT STRUCT(1 AS value).* UNION ALL SELECT id FROM known")
        .unwrap()
        .into_single()
        .unwrap();
    assert!(
        matches!(result.output.shape, Shape::Open { unexpanded } if unexpanded.iter().any(|s| s.relation.is_none()))
    );
}

#[test]
fn nested_star_does_not_open_final_output() {
    let result =
        one("SELECT id FROM some_table JOIN (SELECT * FROM some_unknown_source) src ON 1=1");
    assert_eq!(result.output.shape, Shape::Closed);
    assert!(result.has_unresolved_stars);
    assert_eq!(result.output.columns[0].ordinal, Some(0));
    assert!(matches!(
        result.output.columns[0].lineage,
        Lineage::Complete(_)
    ));
    let named = one("WITH src AS (SELECT * FROM unknown_table) SELECT id FROM src");
    assert_eq!(named.output.shape, Shape::Closed);
    assert!(
        matches!(&named.output.columns[0].lineage, Lineage::Partial { unresolved, .. } if matches!(&unresolved[0], Unresolved::NamedBehindStar { column, .. } if column.value == "id"))
    );
}

#[test]
fn partial_known_unbound_and_ambiguous_sources_are_retained() {
    let result = one("SELECT a.x + missing AS mixed FROM a JOIN b ON 1=1");
    assert!(
        matches!(&result.output.columns[0].lineage, Lineage::Partial { known, unresolved } if known.len() == 1 && matches!(&unresolved[0], Unresolved::Ambiguous { column, .. } if column.value == "missing")),
        "{:?}",
        result.output.columns[0].lineage
    );
    let result = one("SELECT x FROM a JOIN b ON 1=1");
    assert!(
        matches!(&result.output.columns[0].lineage, Lineage::Partial { known, unresolved } if known.is_empty() && matches!(&unresolved[0], Unresolved::Ambiguous { candidates, .. } if candidates.len() == 2))
    );
}

#[test]
fn literals_and_source_free_set_branches_are_complete() {
    assert_eq!(
        one("SELECT 1").output.columns[0].lineage,
        Lineage::Complete(vec![])
    );
    let result = one("SELECT x FROM t UNION ALL SELECT 1");
    assert!(
        matches!(&result.output.columns[0].lineage, Lineage::Complete(sources) if sources.len() == 1)
    );
}

#[test]
fn quoted_source_and_relation_spelling_are_preserved() {
    for (dialect, sql, quote) in [
        (
            Dialect::PostgreSql,
            "SELECT a.\"Mixed\" AS \"Out\" FROM \"Schema\".\"Table\" AS a",
            Quote::Double,
        ),
        (
            Dialect::BigQuery,
            "SELECT a.`Mixed` AS `Out` FROM `Schema`.`Table` AS a",
            Quote::Backtick,
        ),
        (
            Dialect::MsSql,
            "SELECT a.[Mixed] AS [Out] FROM [Schema].[Table] AS a",
            Quote::Bracket,
        ),
    ] {
        let result = Analyzer::new(dialect)
            .analyze(sql)
            .unwrap()
            .into_single()
            .unwrap();
        assert_eq!(result.read[0].name.quote, Some(quote), "{sql}");
        assert!(
            matches!(&result.output.columns[0].name, OutputName::Explicit(id) if id.value == "Out" && id.quote == Some(quote)),
            "{sql}"
        );
        assert!(
            matches!(&result.output.columns[0].lineage, Lineage::Complete(sources) if sources[0].column.value == "Mixed" && sources[0].column.quote == Some(quote)),
            "{sql}: {:?}",
            result.output
        );
    }
}

#[test]
fn row_value_alias_stays_distinct_from_unbound_column() {
    let result = Analyzer::new(Dialect::BigQuery)
        .analyze("SELECT ARRAY_AGG(t) AS rows FROM items t")
        .unwrap()
        .into_single()
        .unwrap();
    assert!(
        matches!(&result.output.columns[0].lineage, Lineage::Partial { unresolved, .. } if matches!(&unresolved[0], Unresolved::RowValue { alias } if alias.value == "t"))
    );
}

#[test]
fn recursive_base_sources_remain_partial() {
    let result = one(
        "WITH RECURSIVE c AS (SELECT id FROM base UNION ALL SELECT id FROM c) SELECT id FROM c",
    );
    assert!(
        matches!(&result.output.columns[0].lineage, Lineage::Partial { known, unresolved } if known.iter().any(|s| s.relation.name.value == "base") && unresolved.contains(&Unresolved::Recursive))
    );
}

struct BorrowedCatalog {
    columns: Vec<CatalogColumn>,
}
impl Catalog for BorrowedCatalog {
    fn lookup(&self, relation: &Relation) -> Option<CatalogEntry<'_>> {
        (relation.name.value.eq_ignore_ascii_case("raw")).then(|| CatalogEntry {
            relation: Relation {
                catalog: None,
                schema: Some(Ident::quoted("Physical", Quote::Double)),
                name: Ident::quoted("Raw", Quote::Double),
            },
            columns: Cow::Borrowed(&self.columns),
        })
    }
}

#[test]
fn borrowed_catalog_expands_in_place_and_preserves_canonical_names() {
    let catalog = BorrowedCatalog {
        columns: vec!["id", "value"]
            .into_iter()
            .map(|name| CatalogColumn {
                name: Ident::unquoted(name),
                data_type: None,
                fields: None,
            })
            .collect(),
    };
    let analyzer = Analyzer::new(Dialect::Generic).with_catalog(&catalog);
    let result = analyzer
        .analyze("SELECT 1 AS prefix, raw.*, 2 AS suffix FROM raw")
        .unwrap()
        .into_single()
        .unwrap();
    assert_eq!(result.output.shape, Shape::Closed);
    assert_eq!(result.output.columns.len(), 4);
    assert_eq!(
        result
            .output
            .columns
            .iter()
            .map(|col| col.ordinal)
            .collect::<Vec<_>>(),
        vec![Some(0), Some(1), Some(2), Some(3)]
    );
    assert!(
        matches!(&result.output.columns[1].lineage, Lineage::Complete(sources) if sources[0].relation.schema.as_ref().unwrap().value == "Physical" && sources[0].relation.name.quote == Some(Quote::Double))
    );
}

#[test]
fn duplicate_names_and_empty_projection_have_diagnostics() {
    let result = one("SELECT x AS same, y AS same FROM t");
    assert!(result.diagnostics.iter().any(|d| matches!(&d.kind, DiagnosticKind::DuplicateOutputName { ordinals, .. } if ordinals == &[0,1])));
    let result = one("SELECT FROM t");
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.kind == DiagnosticKind::EmptyProjection)
    );
}

#[test]
fn bare_from_first_query_is_read_as_select_star_with_a_warning() {
    let result = Analyzer::new(Dialect::DuckDb)
        .analyze("FROM t")
        .unwrap()
        .into_single()
        .unwrap();
    assert_eq!(
        result.diagnostics,
        [Diagnostic {
            severity: Severity::Warning,
            kind: DiagnosticKind::EmptyProjection,
            location: None,
        }]
    );
    let Shape::Open { unexpanded } = &result.output.shape else {
        panic!("a bare FROM is a star over its relation");
    };
    assert_eq!(unexpanded.len(), 1);
    assert_eq!(unexpanded[0].relation.as_ref().unwrap().name.value, "t");

    let selected = Analyzer::new(Dialect::DuckDb)
        .analyze("FROM t SELECT a")
        .unwrap()
        .into_single()
        .unwrap();
    assert!(selected.diagnostics.is_empty());
}

#[test]
fn empty_projection_is_reported_next_to_a_semantic_error() {
    let result = one("SELECT FROM t UNION ALL SELECT a, b FROM u");
    assert!(result.output.columns.is_empty());
    let kinds = result
        .diagnostics
        .iter()
        .map(|d| (d.severity, d.kind.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            (
                Severity::Error,
                DiagnosticKind::SetOperationArityMismatch { left: 0, right: 2 }
            ),
            (Severity::Warning, DiagnosticKind::EmptyProjection),
        ]
    );
}

#[test]
fn write_targets_keep_quotes_and_original_spelling() {
    for sql in [
        "INSERT INTO \"Schema\".\"Target\" SELECT id FROM src",
        "CREATE TABLE \"Schema\".\"Target\" AS SELECT id FROM src",
        "UPDATE \"Schema\".\"Target\" SET id = 1",
        "DELETE FROM \"Schema\".\"Target\"",
    ] {
        let result = one(sql);
        let target = result.written.unwrap();
        assert_eq!(target.name, Ident::quoted("Target", Quote::Double), "{sql}");
        assert_eq!(
            target.schema,
            Some(Ident::quoted("Schema", Quote::Double)),
            "{sql}"
        );
    }
}

#[test]
fn quote_styles_after_catalog_star_expansion_stay_positional() {
    let catalog = BorrowedCatalog {
        columns: vec![CatalogColumn {
            name: Ident::unquoted("id"),
            data_type: None,
            fields: None,
        }],
    };
    let result = Analyzer::new(Dialect::Generic)
        .with_catalog(&catalog)
        .analyze("SELECT raw.*, 1 AS \"id\", 2 AS id FROM raw")
        .unwrap()
        .into_single()
        .unwrap();
    assert_eq!(
        result.output.columns[1].name,
        OutputName::Explicit(Ident::quoted("id", Quote::Double))
    );
    assert_eq!(
        result.output.columns[2].name,
        OutputName::Explicit(Ident::unquoted("id"))
    );
}

#[test]
fn parser_location_is_reported_when_sqlparser_provides_it() {
    let error = parse("SELECT\n)", Dialect::Generic).unwrap_err();
    assert_eq!(error.kind, ParseErrorKind::Syntax);
    assert_eq!(error.location, Some(Location { line: 2, column: 1 }));
}

#[test]
fn source_quote_context_is_specific_to_each_output() {
    let result = one("SELECT a.x AS first, b.\"x\" AS second FROM t AS a JOIN t AS b ON 1=1");
    let Lineage::Complete(first) = &result.output.columns[0].lineage else {
        panic!("expected complete lineage")
    };
    let Lineage::Complete(second) = &result.output.columns[1].lineage else {
        panic!("expected complete lineage")
    };
    assert_eq!(first[0].column, Ident::unquoted("x"));
    assert_eq!(second[0].column, Ident::quoted("x", Quote::Double));
}

#[test]
fn ambiguous_sources_and_known_sources_have_stable_order() {
    let analyzer = Analyzer::new(Dialect::Generic);
    let sql = "SELECT z.x + a.y AS value, missing FROM z JOIN a ON 1=1";
    let expected = analyzer.analyze(sql).unwrap();
    for _ in 0..50 {
        assert_eq!(analyzer.analyze(sql).unwrap(), expected);
    }
    let Lineage::Complete(sources) = &expected.statements[0].output.columns[0].lineage else {
        panic!("expected complete lineage")
    };
    assert_eq!(
        sources
            .iter()
            .map(|s| s.relation.name.value.as_str())
            .collect::<Vec<_>>(),
        ["a", "z"]
    );
    let Lineage::Partial { unresolved, .. } = &expected.statements[0].output.columns[1].lineage
    else {
        panic!("expected ambiguous lineage")
    };
    assert!(
        matches!(&unresolved[0], Unresolved::Ambiguous { candidates, .. } if matches!(&candidates[0], Candidate::Relation(r) if r.name.value == "a"))
    );
}

#[test]
fn duplicate_diagnostics_do_not_invent_ordinals_after_star() {
    let result = one("SELECT *, x AS value, y AS value FROM t");
    assert!(
        matches!(result.output.lookup("value", Dialect::Generic), Lookup::Duplicate(columns) if columns.len() == 2)
    );
    assert!(result.diagnostics.iter().any(|d| matches!(&d.kind, DiagnosticKind::DuplicateOutputName { ordinals, .. } if ordinals.is_empty())));
}

#[test]
fn catalog_star_columns_keep_catalog_quote_metadata() {
    let catalog = BorrowedCatalog {
        columns: vec![CatalogColumn {
            name: Ident::quoted("MiXeD", Quote::Double),
            data_type: None,
            fields: None,
        }],
    };
    let result = Analyzer::new(Dialect::Generic)
        .with_catalog(&catalog)
        .analyze("SELECT * FROM raw")
        .unwrap()
        .into_single()
        .unwrap();
    let expected = Ident::quoted("MiXeD", Quote::Double);
    assert_eq!(
        result.output.columns[0].name,
        OutputName::Explicit(expected.clone())
    );
    assert!(
        matches!(&result.output.columns[0].lineage, Lineage::Complete(sources) if sources[0].column == expected)
    );
}

struct QuoteContextCatalog;
impl Catalog for QuoteContextCatalog {
    fn lookup(&self, relation: &Relation) -> Option<CatalogEntry<'_>> {
        let column = match relation.name.value.as_str() {
            "a" => Ident::quoted("id", Quote::Double),
            "b" => Ident::unquoted("ID"),
            _ => return None,
        };
        Some(CatalogEntry {
            relation: relation.clone(),
            columns: Cow::Owned(vec![CatalogColumn {
                name: column,
                data_type: None,
                fields: None,
            }]),
        })
    }
}

#[test]
fn catalog_never_uses_an_unrelated_quote_context_to_choose_a_source() {
    let catalog = QuoteContextCatalog;
    let analyzer = Analyzer::new(Dialect::Snowflake).with_catalog(&catalog);
    let direct = analyzer
        .analyze("SELECT \"id\" FROM a CROSS JOIN b")
        .unwrap()
        .into_single()
        .unwrap();
    assert!(
        matches!(&direct.output.columns[0].lineage, Lineage::Complete(sources) if sources.len() == 1 && sources[0].relation.name.value == "a")
    );
    let mixed = analyzer
        .analyze("SELECT \"id\" FROM a CROSS JOIN b WHERE id > 0")
        .unwrap()
        .into_single()
        .unwrap();
    assert!(
        matches!(&mixed.output.columns[0].lineage, Lineage::Partial { known, unresolved }
        if known.is_empty() && matches!(&unresolved[..], [Unresolved::Ambiguous { column, candidates }]
            if column.quote == Some(Quote::Double) && candidates.len() == 2))
    );
}

#[test]
fn physical_source_quote_survives_cte_and_derived_output_references() {
    for sql in [
        "WITH c AS (SELECT \"id\" FROM t) SELECT id FROM c",
        "WITH c AS (SELECT \"id\" FROM t) SELECT id + 1 AS n FROM c",
        "WITH c AS (SELECT \"id\" FROM t), d AS (SELECT id FROM c) SELECT id FROM d",
        "SELECT id FROM (SELECT \"id\" FROM t) d",
    ] {
        let result = one(sql);
        assert!(
            matches!(&result.output.columns[0].lineage, Lineage::Complete(sources)
            if sources.len() == 1 && sources[0].relation.name.value == "t" && sources[0].column == Ident::quoted("id", Quote::Double)),
            "{sql}: {:?}",
            result.output
        );
    }
}

#[test]
fn star_inherits_explicit_names_from_cte_and_derived_projections() {
    for sql in [
        "WITH c AS (SELECT 1 AS \"*\", 2 AS \"Mixed\") SELECT * FROM c",
        "WITH c AS (SELECT 1 AS \"*\", 2 AS \"Mixed\"), d AS (SELECT * FROM c) SELECT * FROM d",
        "SELECT * FROM (SELECT 1 AS \"*\", 2 AS \"Mixed\") d",
    ] {
        let result = one(sql);
        assert_eq!(result.output.shape, Shape::Closed, "{sql}");
        assert_eq!(result.output.columns.len(), 2, "{sql}");
        assert_eq!(
            result.output.columns[0].name,
            OutputName::Explicit(Ident::quoted("*", Quote::Double)),
            "{sql}"
        );
        assert_eq!(
            result.output.columns[1].name,
            OutputName::Explicit(Ident::quoted("Mixed", Quote::Double)),
            "{sql}"
        );
    }
}

#[test]
fn cte_candidates_are_distinct_from_physical_relations() {
    let result =
        one("WITH c AS (SELECT id FROM t), d AS (SELECT id FROM u) SELECT id FROM c CROSS JOIN d");
    assert!(
        matches!(&result.output.columns[0].lineage, Lineage::Partial { unresolved, .. }
        if matches!(&unresolved[..], [Unresolved::Ambiguous { candidates, .. }]
            if candidates == &vec![Candidate::Cte(Ident::unquoted("c")), Candidate::Cte(Ident::unquoted("d"))]))
    );
}

#[test]
fn set_operands_keep_their_own_physical_source_quote_context() {
    let result = one("SELECT \"x\" FROM t UNION ALL SELECT x FROM u");
    let Lineage::Complete(sources) = &result.output.columns[0].lineage else {
        panic!("expected complete lineage")
    };
    assert_eq!(
        sources
            .iter()
            .find(|s| s.relation.name.value == "t")
            .unwrap()
            .column,
        Ident::quoted("x", Quote::Double)
    );
    assert_eq!(
        sources
            .iter()
            .find(|s| s.relation.name.value == "u")
            .unwrap()
            .column,
        Ident::unquoted("x")
    );
}

#[test]
fn source_relation_spelling_is_recovered_from_each_qualified_output() {
    let result = one("SELECT a.x AS first, b.y AS second FROM Foo AS a JOIN foo AS b ON 1=1");
    assert_eq!(
        result
            .read
            .iter()
            .map(|r| r.name.value.as_str())
            .collect::<Vec<_>>(),
        ["Foo", "foo"]
    );
    assert!(
        matches!(&result.output.columns[0].lineage, Lineage::Complete(sources) if sources[0].relation.name.value == "Foo")
    );
    assert!(
        matches!(&result.output.columns[1].lineage, Lineage::Complete(sources) if sources[0].relation.name.value == "foo")
    );
}

#[test]
fn nonrecursive_cte_definition_can_read_a_physical_table_with_the_same_name() {
    let result = one("WITH T AS (SELECT \"id\" FROM T) SELECT id FROM T");
    assert_eq!(result.read[0].name, Ident::unquoted("T"));
    assert!(
        matches!(&result.output.columns[0].lineage, Lineage::Complete(sources)
        if sources[0].relation.name == Ident::unquoted("T") && sources[0].column == Ident::quoted("id", Quote::Double))
    );
}

#[test]
fn unresolved_source_quotes_do_not_come_from_output_aliases() {
    let catalog = QuoteContextCatalog;
    let result = Analyzer::new(Dialect::Snowflake)
        .with_catalog(&catalog)
        .analyze("SELECT \"id\" AS output FROM a CROSS JOIN b WHERE id > 0")
        .unwrap()
        .into_single()
        .unwrap();
    assert!(
        matches!(&result.output.columns[0].lineage, Lineage::Partial { unresolved, .. }
        if matches!(&unresolved[..], [Unresolved::Ambiguous { column, .. }] if column.quote == Some(Quote::Double)))
    );
    let result = one("SELECT missing AS \"missing\"");
    assert!(
        matches!(&result.output.columns[0].lineage, Lineage::Partial { unresolved, .. }
        if matches!(&unresolved[..], [Unresolved::Unbound { column }] if column.quote.is_none()))
    );
}

#[test]
fn mixed_cte_star_keeps_an_explicit_star_named_column() {
    let result = one("WITH c AS (SELECT 1 AS \"*\") SELECT c.*, 2 AS tail FROM c");
    assert_eq!(
        result.output.columns[0].name,
        OutputName::Explicit(Ident::quoted("*", Quote::Double))
    );
    assert_eq!(
        result.output.columns[1].name,
        OutputName::Explicit(Ident::unquoted("tail"))
    );
}

#[test]
fn physical_table_in_a_cte_definition_does_not_replace_outer_cte_candidates() {
    let result =
        one("WITH c AS (SELECT id FROM c), d AS (SELECT id FROM u) SELECT id FROM c CROSS JOIN d");
    assert!(
        matches!(&result.output.columns[0].lineage, Lineage::Partial { unresolved, .. }
        if matches!(&unresolved[..], [Unresolved::Ambiguous { candidates, .. }]
            if candidates == &vec![Candidate::Cte(Ident::unquoted("c")), Candidate::Cte(Ident::unquoted("d"))]))
    );
}
