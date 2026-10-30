use dlin_sqllineage::{
    Analyzer, Dialect, Ident, Lineage, OutputName, Quote, Relation, SourceColumn,
};

fn quoted(value: &str) -> Ident {
    Ident::quoted(value, Quote::Double)
}

fn source(table: Ident, column: Ident) -> SourceColumn {
    SourceColumn {
        relation: Relation {
            catalog: None,
            schema: None,
            name: table,
        },
        column,
    }
}

#[test]
fn projection_references_keep_distinct_quote_contexts() {
    for dialect in [Dialect::Generic, Dialect::Snowflake, Dialect::PostgreSql] {
        let result = Analyzer::new(dialect)
            .analyze("SELECT t.\"id\" AS quoted_id, t.ID AS plain_id FROM \"Source.Table\" AS t")
            .unwrap()
            .into_single()
            .unwrap();
        assert_eq!(result.output.columns.len(), 2);
        for (index, column) in [quoted("id"), Ident::unquoted("ID")]
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                result.output.columns[index].lineage,
                Lineage::Complete(vec![source(quoted("Source.Table"), column)]),
                "{dialect}"
            );
        }
    }
}

#[test]
fn cte_output_lookup_uses_column_keys_without_losing_source_quotes() {
    let result = Analyzer::new(Dialect::Snowflake)
        .analyze("WITH c AS (SELECT \"a\" AS \"id\", b AS ID FROM t) SELECT c.\"id\", c.id FROM c")
        .unwrap()
        .into_single()
        .unwrap();
    assert_eq!(result.output.columns.len(), 2);
    assert_eq!(
        result.output.columns[0].name,
        OutputName::Explicit(quoted("id"))
    );
    assert_eq!(
        result.output.columns[1].name,
        OutputName::Explicit(Ident::unquoted("id"))
    );
    assert_eq!(
        result.output.columns[0].lineage,
        Lineage::Complete(vec![source(Ident::unquoted("t"), quoted("a"))])
    );
    assert_eq!(
        result.output.columns[1].lineage,
        Lineage::Complete(vec![source(Ident::unquoted("t"), Ident::unquoted("b"))])
    );
}

#[test]
fn relation_alias_lookup_keeps_quoted_lowercase_distinct() {
    let result = Analyzer::new(Dialect::Snowflake)
        .analyze("SELECT \"x\".id AS a_id, X.id AS b_id FROM a AS \"x\" CROSS JOIN b AS X")
        .unwrap()
        .into_single()
        .unwrap();
    assert_eq!(result.output.columns.len(), 2);
    for (index, table) in ["a", "b"].into_iter().enumerate() {
        assert_eq!(
            result.output.columns[index].lineage,
            Lineage::Complete(vec![source(Ident::unquoted(table), Ident::unquoted("id"))])
        );
    }
}

#[test]
fn virtual_column_alias_lookup_follows_column_case_rules() {
    let result = Analyzer::new(Dialect::BigQuery)
        .analyze("SELECT item FROM UNNEST([1, 2]) AS ITEM")
        .unwrap()
        .into_single()
        .unwrap();
    assert_eq!(result.output.columns.len(), 1);
    assert_eq!(result.output.columns[0].lineage, Lineage::Complete(vec![]));
}

#[test]
fn folded_reference_does_not_match_a_distinct_quoted_cte_output() {
    for dialect in [Dialect::Generic, Dialect::Snowflake] {
        let result = Analyzer::new(dialect)
            .analyze("WITH c AS (SELECT \"id\" FROM t) SELECT id FROM c")
            .unwrap()
            .into_single()
            .unwrap();
        assert_eq!(result.output.columns.len(), 1);
        assert_eq!(
            result.output.columns[0].lineage,
            Lineage::Partial {
                known: vec![],
                unresolved: vec![dlin_sqllineage::Unresolved::Unbound {
                    column: Ident::unquoted("id")
                }],
            },
            "{dialect}"
        );
    }
}

#[test]
fn update_ambiguity_keeps_derived_candidate_kinds() {
    for sql in [
        "UPDATE dest SET result_col = id FROM (SELECT a AS id FROM s) AS d1 CROSS JOIN (SELECT b AS id FROM t) AS d2",
        "UPDATE dest SET result_col = id FROM (SELECT a AS id FROM s) AS d1 CROSS JOIN (SELECT b AS id FROM (SELECT b FROM t) AS d1) AS d2",
    ] {
        let result = Analyzer::new(Dialect::PostgreSql)
            .analyze(sql)
            .unwrap()
            .into_single()
            .unwrap();
        assert_eq!(result.output.columns.len(), 1);
        assert_eq!(
            result.output.columns[0].lineage,
            Lineage::Partial {
                known: vec![],
                unresolved: vec![dlin_sqllineage::Unresolved::Ambiguous {
                    column: Ident::unquoted("id"),
                    candidates: vec![
                        dlin_sqllineage::Candidate::Derived(Some(Ident::unquoted("d1"))),
                        dlin_sqllineage::Candidate::Derived(Some(Ident::unquoted("d2"))),
                    ],
                }],
            }
        );
    }
}

struct OneColumnCatalog;

impl dlin_sqllineage::Catalog for OneColumnCatalog {
    fn lookup(&self, relation: &Relation) -> Option<dlin_sqllineage::CatalogEntry<'_>> {
        (relation.name == Ident::unquoted("t")).then(|| dlin_sqllineage::CatalogEntry {
            relation: relation.clone(),
            columns: std::borrow::Cow::Owned(vec![dlin_sqllineage::CatalogColumn {
                name: Ident::unquoted("id"),
                data_type: None,
                fields: None,
            }]),
        })
    }
}

#[test]
fn repeated_stars_do_not_copy_an_equivalent_alias_quote() {
    let result = Analyzer::new(Dialect::Snowflake)
        .with_catalog(&OneColumnCatalog)
        .analyze("SELECT t.*, 1 AS \"ID\", t.* FROM t")
        .unwrap()
        .into_single()
        .unwrap();
    assert_eq!(result.output.columns.len(), 3);
    for (index, name) in [Ident::unquoted("id"), quoted("ID"), Ident::unquoted("id")]
        .into_iter()
        .enumerate()
    {
        assert_eq!(result.output.columns[index].ordinal, Some(index));
        assert_eq!(
            result.output.columns[index].name,
            OutputName::Explicit(name)
        );
        let sources = if index == 1 {
            vec![]
        } else {
            vec![source(Ident::unquoted("t"), Ident::unquoted("id"))]
        };
        assert_eq!(
            result.output.columns[index].lineage,
            Lineage::Complete(sources)
        );
    }
    assert_eq!(
        result.diagnostics,
        vec![dlin_sqllineage::Diagnostic {
            severity: dlin_sqllineage::Severity::Warning,
            kind: dlin_sqllineage::DiagnosticKind::DuplicateOutputName {
                name: Ident::unquoted("id"),
                ordinals: vec![0, 1, 2]
            },
            location: None,
        }]
    );
    assert!(!result.has_unresolved_stars);
}

#[test]
fn quoted_star_is_a_named_column_in_set_operations() {
    for sql in [
        "SELECT \"*\" UNION SELECT id FROM t",
        "SELECT \"*\" UNION ALL SELECT id FROM t",
    ] {
        let result = Analyzer::new(Dialect::Generic)
            .analyze(sql)
            .unwrap()
            .into_single()
            .unwrap();
        assert_eq!(result.output.columns.len(), 1, "{sql}");
        assert_eq!(
            result.output.columns[0].name,
            OutputName::Explicit(quoted("*")),
            "{sql}"
        );
        assert_eq!(result.output.columns[0].ordinal, Some(0), "{sql}");
        assert_eq!(
            result.output.columns[0].lineage,
            Lineage::Partial {
                known: vec![source(Ident::unquoted("t"), Ident::unquoted("id"))],
                unresolved: vec![dlin_sqllineage::Unresolved::Unbound {
                    column: quoted("*")
                }],
            },
            "{sql}"
        );
        assert_eq!(result.output.shape, dlin_sqllineage::Shape::Closed, "{sql}");
        assert!(!result.has_unresolved_stars, "{sql}");
    }
}
