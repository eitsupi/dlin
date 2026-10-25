use crate::types::Dialect;
use sqlparser::dialect::{
    self, AnsiDialect, BigQueryDialect, ClickHouseDialect, DatabricksDialect, DuckDbDialect,
    GenericDialect, HiveDialect, MsSqlDialect, MySqlDialect, PostgreSqlDialect, RedshiftSqlDialect,
    SQLiteDialect, SnowflakeDialect, SparkSqlDialect,
};

impl Dialect {
    pub fn to_sqlparser_dialect(&self) -> Box<dyn dialect::Dialect> {
        match self {
            Dialect::Generic => Box::new(GenericDialect),
            Dialect::Ansi => Box::new(AnsiDialect {}),
            Dialect::PostgreSql => Box::new(PostgreSqlDialect {}),
            Dialect::MySql => Box::new(MySqlDialect {}),
            Dialect::Hive => Box::new(HiveDialect {}),
            Dialect::Databricks => Box::new(DatabricksDialect),
            Dialect::Snowflake => Box::new(SnowflakeDialect),
            Dialect::BigQuery => Box::new(BigQueryDialect),
            Dialect::DuckDb => Box::new(DuckDbDialect),
            Dialect::Redshift => Box::new(RedshiftSqlDialect {}),
            // sqlparser 0.62 has no TrinoDialect; GenericDialect accepts the
            // common Trino grammar without pretending to provide dialect-only
            // validation.
            Dialect::Trino => Box::new(GenericDialect),
            Dialect::Spark => Box::new(SparkSqlDialect),
            Dialect::ClickHouse => Box::new(ClickHouseDialect {}),
            Dialect::SQLite => Box::new(SQLiteDialect {}),
            Dialect::MsSql => Box::new(MsSqlDialect {}),
        }
    }
}
