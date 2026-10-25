use sqllineage::{AnalyzeOptions, Dialect, StatementType, analyze};

#[test]
fn all_public_dialects_parse_basic_queries() {
    let dialects = [
        Dialect::Generic,
        Dialect::Ansi,
        Dialect::PostgreSql,
        Dialect::MySql,
        Dialect::Hive,
        Dialect::Databricks,
        Dialect::Snowflake,
        Dialect::BigQuery,
        Dialect::DuckDb,
        Dialect::Redshift,
        Dialect::Trino,
        Dialect::Spark,
        Dialect::ClickHouse,
        Dialect::SQLite,
        Dialect::MsSql,
    ];

    for dialect in dialects {
        let result = analyze(
            "SELECT 1",
            AnalyzeOptions {
                dialect,
                ..AnalyzeOptions::default()
            },
        )
        .expect("basic query should parse for every public dialect");
        assert_eq!(result[0].statement_type, StatementType::Query);
    }
}
