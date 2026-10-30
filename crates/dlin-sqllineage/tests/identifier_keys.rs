use dlin_sqllineage::{
    Analyzer, Candidate, Dialect, Ident, Lineage, Quote, Relation, SourceColumn, Unresolved,
};

fn id(value: &str) -> Ident {
    Ident::unquoted(value)
}

fn quoted(value: &str, quote: Quote) -> Ident {
    Ident::quoted(value, quote)
}

fn relation(catalog: Option<Ident>, schema: Option<Ident>, name: Ident) -> Relation {
    Relation {
        catalog,
        schema,
        name,
    }
}

fn one(dialect: Dialect, sql: &str) -> dlin_sqllineage::StatementAnalysis {
    Analyzer::new(dialect)
        .analyze(sql)
        .unwrap_or_else(|error| panic!("{dialect:?}: {sql}: {error:?}"))
        .into_single()
        .unwrap()
}

#[test]
fn read_relations_sort_and_deduplicate_by_dialect_relation_keys() {
    let snowflake = one(
        Dialect::Snowflake,
        "SELECT 1 FROM z.db.t JOIN Z.DB.T AS same ON 1 = 1 JOIN \"B\".db.t AS quoted_name ON 1 = 1 JOIN a.db.t AS earlier ON 1 = 1",
    );
    assert_eq!(
        snowflake.read,
        vec![
            relation(Some(id("a")), Some(id("db")), id("t")),
            relation(Some(quoted("B", Quote::Double)), Some(id("db")), id("t")),
            relation(Some(id("z")), Some(id("db")), id("t")),
        ]
    );

    let postgres = one(
        Dialect::PostgreSql,
        "SELECT 1 FROM Z.Db.T JOIN z.db.t AS same ON 1 = 1 JOIN A.db.t AS earlier ON 1 = 1",
    );
    assert_eq!(
        postgres.read,
        vec![
            relation(Some(id("A")), Some(id("db")), id("t")),
            relation(Some(id("Z")), Some(id("Db")), id("T")),
        ]
    );

    // BigQuery relation keys are exact even for unquoted identifiers, so
    // differently cased relation spellings remain distinct.
    let bigquery = one(
        Dialect::BigQuery,
        "SELECT 1 FROM a.db.t JOIN A.db.t AS upper_name ON 1 = 1 JOIN a.db.t AS duplicate ON 1 = 1",
    );
    assert_eq!(
        bigquery.read,
        vec![
            relation(Some(id("A")), Some(id("db")), id("t")),
            relation(Some(id("a")), Some(id("db")), id("t")),
        ]
    );
}

#[test]
fn semantic_failure_reads_use_relation_keys_for_sorting_and_deduplication() {
    let result = one(
        Dialect::Snowflake,
        "SELECT x FROM z.db.t JOIN \"B\".db.t AS quoted_name ON 1 = 1 JOIN Z.DB.T AS duplicate ON 1 = 1 UNION ALL SELECT x, y FROM a.db.t",
    );

    assert!(result.output.columns.is_empty());
    assert_eq!(
        result.diagnostics[0].kind,
        dlin_sqllineage::DiagnosticKind::SetOperationArityMismatch { left: 1, right: 2 }
    );
    // The exact quoted B key sorts between folded unquoted A and Z. Raw
    // spelling order would place B before the unquoted spelling a.
    assert_eq!(
        result.read,
        vec![
            relation(Some(id("a")), Some(id("db")), id("t")),
            relation(Some(quoted("B", Quote::Double)), Some(id("db")), id("t")),
            relation(Some(id("z")), Some(id("db")), id("t")),
        ]
    );
}

#[test]
fn complete_sources_fold_and_deduplicate_with_first_from_relation_spelling() {
    // The expression visits the second alias first. Both FROM entries have
    // the same Snowflake relation key, so first FROM spelling must win.
    let snowflake = one(
        Dialect::Snowflake,
        "SELECT b.X + a.x AS value FROM z.db.t AS a JOIN Z.DB.T AS b ON 1 = 1",
    );
    assert_eq!(
        snowflake.read,
        vec![relation(Some(id("z")), Some(id("db")), id("t"))]
    );
    assert_eq!(
        snowflake.output.columns[0].lineage,
        Lineage::Complete(vec![SourceColumn {
            relation: relation(Some(id("z")), Some(id("db")), id("t")),
            column: id("X"),
        }])
    );

    // Reversing the FROM entries reverses which exact spelling represents
    // their shared semantic relation key, independently of expression order.
    let snowflake_reversed_from = one(
        Dialect::Snowflake,
        "SELECT b.X + a.x AS value FROM Z.DB.T AS b JOIN z.db.t AS a ON 1 = 1",
    );
    assert_eq!(
        snowflake_reversed_from.output.columns[0].lineage,
        Lineage::Complete(vec![SourceColumn {
            relation: relation(Some(id("Z")), Some(id("DB")), id("T")),
            column: id("X"),
        }])
    );

    let postgres = one(
        Dialect::PostgreSql,
        "SELECT b.X + a.x AS value FROM Z.Db.t AS a JOIN z.db.t AS b ON 1 = 1",
    );
    assert_eq!(
        postgres.output.columns[0].lineage,
        Lineage::Complete(vec![SourceColumn {
            relation: relation(Some(id("Z")), Some(id("Db")), id("t")),
            column: id("X"),
        }])
    );

    let snowflake_quoted = one(
        Dialect::Snowflake,
        "SELECT a.id + b.id AS value FROM t AS a JOIN \"t\" AS b ON 1 = 1",
    );
    assert_eq!(
        snowflake_quoted.output.columns[0].lineage,
        Lineage::Complete(vec![
            SourceColumn {
                relation: relation(None, None, id("t")),
                column: id("id"),
            },
            SourceColumn {
                relation: relation(None, None, quoted("t", Quote::Double)),
                column: id("id"),
            },
        ])
    );

    let postgres_quoted = one(
        Dialect::PostgreSql,
        "SELECT a.id + b.id AS value FROM t AS a JOIN \"T\" AS b ON 1 = 1",
    );
    assert_eq!(
        postgres_quoted.output.columns[0].lineage,
        Lineage::Complete(vec![
            SourceColumn {
                relation: relation(None, None, quoted("T", Quote::Double)),
                column: id("id"),
            },
            SourceColumn {
                relation: relation(None, None, id("t")),
                column: id("id"),
            },
        ])
    );

    let bigquery = one(
        Dialect::BigQuery,
        "SELECT a.`Foo` + a.`fOO` AS value FROM `db`.`t` AS a",
    );
    assert_eq!(
        bigquery.output.columns[0].lineage,
        Lineage::Complete(vec![SourceColumn {
            relation: relation(
                None,
                Some(quoted("db", Quote::Backtick)),
                quoted("t", Quote::Backtick),
            ),
            column: quoted("Foo", Quote::Backtick),
        }])
    );

    let bigquery_cte_first_occurrence = one(
        Dialect::BigQuery,
        "WITH c AS (SELECT Foo AS x, fOO AS y FROM t) SELECT y + x AS value FROM c",
    );
    assert_eq!(
        bigquery_cte_first_occurrence.output.columns[0].lineage,
        Lineage::Complete(vec![SourceColumn {
            relation: relation(None, None, id("t")),
            column: id("Foo"),
        }])
    );

    let bigquery_cte_reversed_definition = one(
        Dialect::BigQuery,
        "WITH c AS (SELECT fOO AS y, Foo AS x FROM t) SELECT y + x AS value FROM c",
    );
    assert_eq!(
        bigquery_cte_reversed_definition.output.columns[0].lineage,
        Lineage::Complete(vec![SourceColumn {
            relation: relation(None, None, id("t")),
            column: id("fOO"),
        }])
    );
}

#[test]
fn partial_known_sources_and_ambiguity_candidates_use_semantic_keys() {
    let bigquery = one(
        Dialect::BigQuery,
        "WITH c AS (SELECT a.`Foo` AS x, a.`fOO` AS y FROM `db`.`t` AS a) SELECT x + y + missing AS value FROM c",
    );
    assert_eq!(
        bigquery.output.columns[0].lineage,
        Lineage::Partial {
            known: vec![SourceColumn {
                relation: relation(
                    None,
                    Some(quoted("db", Quote::Backtick)),
                    quoted("t", Quote::Backtick),
                ),
                column: quoted("Foo", Quote::Backtick),
            }],
            unresolved: vec![Unresolved::Unbound {
                column: id("missing"),
            }],
        }
    );

    let bigquery_ambiguous_column = one(
        Dialect::BigQuery,
        "SELECT Missing + MISSING AS value FROM a CROSS JOIN b",
    );
    assert_eq!(
        bigquery_ambiguous_column.output.columns[0].lineage,
        Lineage::Partial {
            known: vec![],
            unresolved: vec![Unresolved::Ambiguous {
                column: id("Missing"),
                candidates: vec![
                    Candidate::Relation(relation(None, None, id("a"))),
                    Candidate::Relation(relation(None, None, id("b"))),
                ],
            }],
        }
    );

    let snowflake = one(
        Dialect::Snowflake,
        "SELECT id FROM Z.Db.T AS a JOIN z.db.t AS b ON 1 = 1 JOIN A.db.t AS c ON 1 = 1",
    );
    assert_eq!(
        snowflake.output.columns[0].lineage,
        Lineage::Partial {
            known: vec![],
            unresolved: vec![Unresolved::Ambiguous {
                column: id("id"),
                candidates: vec![
                    Candidate::Relation(relation(Some(id("A")), Some(id("db")), id("t"))),
                    Candidate::Relation(relation(Some(id("Z")), Some(id("Db")), id("T"))),
                ],
            }],
        }
    );

    let postgres = one(
        Dialect::PostgreSql,
        "SELECT id FROM Z.Db.T AS a JOIN z.db.t AS b ON 1 = 1 JOIN A.db.t AS c ON 1 = 1",
    );
    assert_eq!(
        postgres.output.columns[0].lineage,
        Lineage::Partial {
            known: vec![],
            unresolved: vec![Unresolved::Ambiguous {
                column: id("id"),
                candidates: vec![
                    Candidate::Relation(relation(Some(id("A")), Some(id("db")), id("t"))),
                    Candidate::Relation(relation(Some(id("Z")), Some(id("Db")), id("T"))),
                ],
            }],
        }
    );

    let bigquery = one(
        Dialect::BigQuery,
        "SELECT id FROM `a`.`db`.`t` AS lower_name JOIN `A`.`db`.`t` AS upper_name ON 1 = 1",
    );
    assert_eq!(
        bigquery.output.columns[0].lineage,
        Lineage::Partial {
            known: vec![],
            unresolved: vec![Unresolved::Ambiguous {
                column: id("id"),
                candidates: vec![
                    Candidate::Relation(relation(
                        Some(quoted("A", Quote::Backtick)),
                        Some(quoted("db", Quote::Backtick)),
                        quoted("t", Quote::Backtick),
                    )),
                    Candidate::Relation(relation(
                        Some(quoted("a", Quote::Backtick)),
                        Some(quoted("db", Quote::Backtick)),
                        quoted("t", Quote::Backtick),
                    )),
                ],
            }],
        }
    );
}

#[test]
fn star_unresolved_entries_deduplicate_by_relation_key_with_first_from_spelling() {
    // Both CTEs read the same Snowflake relation, so one unresolved entry
    // remains, spelled like the first FROM occurrence and first column reference.
    let from_upper_first = one(
        Dialect::Snowflake,
        "WITH c AS (SELECT * FROM Z.Db.T), d AS (SELECT * FROM z.db.t) SELECT c.id + d.ID FROM c JOIN d ON 1 = 1",
    );
    assert_eq!(
        from_upper_first.output.columns[0].lineage,
        Lineage::Partial {
            known: vec![],
            unresolved: vec![Unresolved::NamedBehindStar {
                relation: relation(Some(id("Z")), Some(id("Db")), id("T")),
                column: id("id"),
            }],
        }
    );

    let from_lower_first = one(
        Dialect::Snowflake,
        "WITH d AS (SELECT * FROM z.db.t), c AS (SELECT * FROM Z.Db.T) SELECT d.id + c.ID FROM c JOIN d ON 1 = 1",
    );
    assert_eq!(
        from_lower_first.output.columns[0].lineage,
        Lineage::Partial {
            known: vec![],
            unresolved: vec![Unresolved::NamedBehindStar {
                relation: relation(Some(id("z")), Some(id("db")), id("t")),
                column: id("id"),
            }],
        }
    );
}
