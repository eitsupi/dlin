use super::support::*;
use dlin_sqllineage::{Candidate, Dialect, Ident, Quote, Relation, Unresolved};

pub fn cases() -> Vec<Case> {
    let mut cases = Vec::new();

    // The lowercase quoted column and the uppercase unquoted column are
    // different columns in these dialects, even when used in the same SQL.
    for (prefix, dialect) in [
        ("snowflake", Dialect::Snowflake),
        ("generic", Dialect::Generic),
        ("ansi", Dialect::Ansi),
    ] {
        let catalog = vec![
            (relation("a"), vec![q("id")]),
            (relation("b"), vec![u("ID")]),
        ];
        cases.push(
            resolved(
                &format!("{prefix}_unquoted_folds_upper"),
                "SELECT id FROM a CROSS JOIN b",
                dialect,
                u("id"),
                relation("b"),
                u("ID"),
            )
            .catalog(catalog.clone())
            .known_failure("Catalog-resolved source columns retain the reference spelling instead of the catalog spelling."),
        );
        cases.push(
            resolved(
                &format!("{prefix}_quoted_lower_is_distinct"),
                "SELECT \"id\" FROM a CROSS JOIN b",
                dialect,
                q("id"),
                relation("a"),
                q("id"),
            )
            .catalog(catalog),
        );
        cases.push(
            resolved(
                &format!("{prefix}_quoted_mixed_is_exact"),
                "SELECT \"MiXeD\" FROM a CROSS JOIN b",
                dialect,
                q("MiXeD"),
                relation("a"),
                q("MiXeD"),
            )
            .catalog(vec![
                (relation("a"), vec![q("MiXeD")]),
                (relation("b"), vec![u("MIXED")]),
            ]),
        );
    }

    cases.push(
        resolved(
            "postgres_unquoted_folds_lower",
            "SELECT ID FROM a CROSS JOIN b",
            Dialect::PostgreSql,
            u("ID"),
            relation("b"),
            u("id"),
        )
        .catalog(vec![
            (relation("a"), vec![q("ID")]),
            (relation("b"), vec![u("id")]),
        ])
        .known_failure("Catalog-resolved source columns retain the reference spelling instead of the catalog spelling."),
    );
    cases.push(
        resolved(
            "postgres_quoted_upper_is_distinct",
            "SELECT \"ID\" FROM a CROSS JOIN b",
            Dialect::PostgreSql,
            q("ID"),
            relation("a"),
            q("ID"),
        )
        .catalog(vec![
            (relation("a"), vec![q("ID")]),
            (relation("b"), vec![u("id")]),
        ]),
    );
    cases.push(
        resolved(
            "postgres_quoted_mixed_is_exact",
            "SELECT \"MiXeD\" FROM a CROSS JOIN b",
            Dialect::PostgreSql,
            q("MiXeD"),
            relation("a"),
            q("MiXeD"),
        )
        .catalog(vec![
            (relation("a"), vec![q("MiXeD")]),
            (relation("b"), vec![u("mixed")]),
        ]),
    );

    // Quoting does not enable case-sensitive column matching here. The
    // physical source still uses the catalog's exact spelling and quote.
    for (prefix, dialect, quote, quoted_sql) in [
        (
            "bigquery",
            Dialect::BigQuery,
            Quote::Backtick,
            "SELECT `MiXeD` FROM a CROSS JOIN b",
        ),
        (
            "duckdb",
            Dialect::DuckDb,
            Quote::Double,
            "SELECT \"MiXeD\" FROM a CROSS JOIN b",
        ),
        (
            "mysql",
            Dialect::MySql,
            Quote::Backtick,
            "SELECT `MiXeD` FROM a CROSS JOIN b",
        ),
    ] {
        let canonical = Ident::quoted("MiXeD", quote);
        let catalog = vec![
            (relation("a"), vec![canonical.clone()]),
            (relation("b"), vec![u("other")]),
        ];
        cases.push(
            resolved(
                &format!("{prefix}_unquoted_matches_quoted_ignoring_case"),
                "SELECT mixed FROM a CROSS JOIN b",
                dialect,
                u("mixed"),
                relation("a"),
                canonical.clone(),
            )
            .catalog(catalog.clone())
            .known_failure("Catalog-resolved source columns lose the catalog's canonical spelling and quoting."),
        );
        cases.push(
            resolved(
                &format!("{prefix}_quoted_catalog_spelling"),
                quoted_sql,
                dialect,
                canonical.clone(),
                relation("a"),
                canonical,
            )
            .catalog(catalog),
        );
        let reference = Ident::quoted("mIxEd", quote);
        let sql = match quote {
            Quote::Double => "SELECT \"mIxEd\" FROM a CROSS JOIN b",
            _ => "SELECT `mIxEd` FROM a CROSS JOIN b",
        };
        cases.push(
            resolved(
                &format!("{prefix}_quoted_reference_ignores_case"),
                sql,
                dialect,
                reference,
                relation("a"),
                Ident::quoted("MiXeD", quote),
            )
            .catalog(vec![
                (relation("a"), vec![Ident::quoted("MiXeD", quote)]),
                (relation("b"), vec![u("other")]),
            ])
            .known_failure("Catalog-resolved source columns retain the quoted reference spelling instead of the catalog spelling."),
        );
    }

    for (name, sql, column, table) in [
        (
            "clickhouse_upper_exact",
            "SELECT Foo FROM a CROSS JOIN b",
            "Foo",
            "a",
        ),
        (
            "clickhouse_lower_exact",
            "SELECT foo FROM a CROSS JOIN b",
            "foo",
            "b",
        ),
    ] {
        cases.push(
            resolved(
                name,
                sql,
                Dialect::ClickHouse,
                u(column),
                relation(table),
                u(column),
            )
            .catalog(vec![
                (relation("a"), vec![u("Foo")]),
                (relation("b"), vec![u("foo")]),
            ]),
        );
    }

    cases.extend([
        resolved(
            "snowflake_where_quote_context_does_not_change_projection_binding",
            "SELECT \"id\" FROM a CROSS JOIN b WHERE id > 0",
            Dialect::Snowflake,
            q("id"),
            relation("a"),
            q("id"),
        )
        .catalog(vec![
            (relation("a"), vec![q("id")]),
            (relation("b"), vec![u("ID")]),
        ])
        .known_failure("Statement-wide quote collection lets an unrelated unquoted WHERE reference make the quoted projection ambiguous."),
        resolved(
            "generic_natural_join_does_not_merge_distinct_column_keys",
            "SELECT id FROM a NATURAL JOIN b",
            Dialect::Generic,
            u("id"),
            relation("b"),
            u("ID"),
        )
        .catalog(vec![
            (relation("a"), vec![q("id")]),
            (relation("b"), vec![u("ID")]),
        ])
        .known_failure("NATURAL membership conflates quoted lowercase with unquoted uppercase, and catalog source spelling is not canonicalized."),
        resolved(
            "generic_natural_membership_without_spelling_difference",
            "SELECT ID FROM a NATURAL JOIN b",
            Dialect::Generic,
            u("ID"),
            relation("b"),
            u("ID"),
        )
        .catalog(vec![
            (relation("a"), vec![q("id")]),
            (relation("b"), vec![u("ID")]),
        ])
        .known_failure("NATURAL column membership conflates the distinct quoted lowercase and unquoted uppercase column keys."),
        resolved(
            "qualified_column_is_not_its_table_name",
            "SELECT T.id FROM T",
            Dialect::Generic,
            u("id"),
            relation("T"),
            u("id"),
        )
        .known_failure("A qualified reference matching its table name is reconstructed with source column T instead of id."),
        resolved(
            "catalog_unquoted_canonical_column_spelling",
            "SELECT amount FROM invoices",
            Dialect::Generic,
            u("amount"),
            relation("invoices"),
            u("AMOUNT"),
        )
        .catalog(vec![(relation("invoices"), vec![u("AMOUNT")])])
        .known_failure("Catalog-resolved source columns retain the reference spelling instead of the catalog spelling."),
        resolved(
            "catalog_disambiguates_unqualified_column",
            "SELECT id FROM a CROSS JOIN b",
            Dialect::Generic,
            u("id"),
            relation("b"),
            u("id"),
        )
        .catalog(vec![
            (relation("a"), vec![u("name")]),
            (relation("b"), vec![u("id")]),
        ]),
        resolved(
            "unknown_table_single_source",
            "SELECT id FROM missing",
            Dialect::Generic,
            u("id"),
            relation("missing"),
            u("id"),
        )
        .catalog(vec![(relation("known"), vec![u("id")])]),
        resolved(
            "unknown_table_qualified_column",
            "SELECT m.id FROM missing AS m",
            Dialect::Generic,
            u("id"),
            relation("missing"),
            u("id"),
        )
        .catalog(vec![(relation("known"), vec![u("id")])]),
        ambiguous(
            "catalog_both_relations_have_column",
            "SELECT id FROM a CROSS JOIN b",
            vec![relation("a"), relation("b")],
        )
        .catalog(vec![
            (relation("a"), vec![u("id")]),
            (relation("b"), vec![u("id")]),
        ]),
        ambiguous(
            "unknown_tables_do_not_choose_arbitrary_source",
            "SELECT id FROM a CROSS JOIN b",
            vec![relation("a"), relation("b")],
        ),
    ]);

    let sales = schema_table(u("sales"), u("orders"));
    let archive = schema_table(u("archive"), u("orders"));
    cases.extend([
        resolved(
            "schema_table_qualified_reference",
            "SELECT orders.id FROM sales.orders",
            Dialect::Generic,
            u("id"),
            sales.clone(),
            u("id"),
        )
        .catalog(vec![(sales.clone(), vec![u("id")])]),
        resolved(
            "schema_table_alias_selects_physical_relation",
            "SELECT archived.id FROM sales.orders AS current_orders CROSS JOIN archive.orders AS archived",
            Dialect::Generic,
            u("id"),
            archive.clone(),
            u("id"),
        )
        .catalog(vec![
            (sales.clone(), vec![u("id")]),
            (archive.clone(), vec![u("id")]),
        ]),
        ambiguous(
            "same_table_name_different_schemas_remains_ambiguous",
            "SELECT id FROM sales.orders AS current_orders CROSS JOIN archive.orders AS archived",
            vec![archive.clone(), sales.clone()],
        )
        .catalog(vec![
            (sales, vec![u("id")]),
            (archive, vec![u("id")]),
        ]),
        resolved(
            "quoted_schema_table_and_column_preserve_spelling",
            "SELECT o.\"OrderId\" FROM \"Sales\".\"Orders\" AS o",
            Dialect::Snowflake,
            q("OrderId"),
            schema_table(q("Sales"), q("Orders")),
            q("OrderId"),
        ).catalog(vec![(schema_table(q("Sales"), q("Orders")), vec![q("OrderId")])]),
        resolved(
            "catalog_returns_canonical_qualified_relation",
            "SELECT id FROM orders",
            Dialect::Generic,
            u("id"),
            relation("warehouse.sales.orders"),
            u("id"),
        ).catalog_alias(relation("orders"), relation("warehouse.sales.orders"), vec![u("id")]),
        resolved(
            "catalog_returns_canonical_quoted_relation",
            "SELECT id FROM orders",
            Dialect::Generic,
            u("id"),
            schema_table(q("Sales"), q("Orders")),
            u("id"),
        ).catalog_alias(relation("orders"), schema_table(q("Sales"), q("Orders")), vec![u("id")]),
    ]);
    cases
}

fn resolved(
    name: &str,
    sql: &str,
    dialect: Dialect,
    label: Ident,
    table: Relation,
    column: Ident,
) -> Case {
    Case::new(
        name,
        sql,
        dialect,
        vec![col(0, label, vec![source(table, column)])],
    )
}

fn ambiguous(name: &str, sql: &str, mut tables: Vec<Relation>) -> Case {
    tables.sort();
    Case::new(
        name,
        sql,
        Dialect::Generic,
        vec![partial_col(
            0,
            u("id"),
            vec![],
            vec![Unresolved::Ambiguous {
                column: u("id"),
                candidates: tables.into_iter().map(Candidate::Relation).collect(),
            }],
        )],
    )
}

fn schema_table(schema: Ident, name: Ident) -> Relation {
    Relation {
        catalog: None,
        schema: Some(schema),
        name,
    }
}

#[test]
fn column_keys_follow_each_dialects_identifier_rules() {
    // Expected keys are literal SQL identifier rules, independent of binding
    // and of which quote delimiters a particular parser accepts.
    let rules = [
        (Dialect::Generic, "MIXED", "MiXeD", true, false),
        (Dialect::Ansi, "MIXED", "MiXeD", true, false),
        (Dialect::PostgreSql, "mixed", "MiXeD", false, true),
        (Dialect::MySql, "mixed", "mixed", true, true),
        (Dialect::Hive, "mixed", "mixed", true, true),
        (Dialect::Databricks, "mixed", "mixed", true, true),
        (Dialect::Snowflake, "MIXED", "MiXeD", true, false),
        (Dialect::BigQuery, "mixed", "mixed", true, true),
        (Dialect::DuckDb, "mixed", "mixed", true, true),
        (Dialect::Redshift, "mixed", "mixed", true, true),
        (Dialect::Trino, "mixed", "mixed", true, true),
        (Dialect::Spark, "mixed", "mixed", true, true),
        (Dialect::ClickHouse, "MiXeD", "MiXeD", false, true),
        (Dialect::SQLite, "mixed", "mixed", true, true),
        (Dialect::MsSql, "mixed", "mixed", true, true),
    ];
    assert_eq!(rules.map(|rule| rule.0), Dialect::ALL);
    for (dialect, unquoted_key, quoted_key, matches_upper, matches_lower) in rules {
        assert_eq!(u("MiXeD").column_key(dialect), unquoted_key, "{dialect}");
        for quote in [
            Quote::Double,
            Quote::Backtick,
            Quote::Bracket,
            Quote::Single,
        ] {
            assert_eq!(
                Ident::quoted("MiXeD", quote).column_key(dialect),
                quoted_key,
                "{dialect}: {quote:?}"
            );
            assert_eq!(
                u("id").column_key(dialect) == Ident::quoted("ID", quote).column_key(dialect),
                matches_upper,
                "{dialect}: unquoted id versus quoted ID ({quote:?})"
            );
            assert_eq!(
                u("id").column_key(dialect) == Ident::quoted("id", quote).column_key(dialect),
                matches_lower,
                "{dialect}: unquoted id versus quoted id ({quote:?})"
            );
        }
    }
}
