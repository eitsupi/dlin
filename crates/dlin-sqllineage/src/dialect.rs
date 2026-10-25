use crate::types::Dialect;
use sqlparser::dialect::{
    self, AnsiDialect, BigQueryDialect, ClickHouseDialect, DatabricksDialect, DuckDbDialect,
    GenericDialect, HiveDialect, MsSqlDialect, MySqlDialect, PostgreSqlDialect, RedshiftSqlDialect,
    SQLiteDialect, SnowflakeDialect, SparkSqlDialect,
};
use std::fmt;
use std::str::FromStr;

impl Dialect {
    /// All dialects supported by this crate, in stable order.
    pub const ALL: [Self; 15] = [
        Self::Generic,
        Self::Ansi,
        Self::PostgreSql,
        Self::MySql,
        Self::Hive,
        Self::Databricks,
        Self::Snowflake,
        Self::BigQuery,
        Self::DuckDb,
        Self::Redshift,
        Self::Trino,
        Self::Spark,
        Self::ClickHouse,
        Self::SQLite,
        Self::MsSql,
    ];

    /// Return the canonical lowercase name of this dialect.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::Ansi => "ansi",
            Self::PostgreSql => "postgresql",
            Self::MySql => "mysql",
            Self::Hive => "hive",
            Self::Databricks => "databricks",
            Self::Snowflake => "snowflake",
            Self::BigQuery => "bigquery",
            Self::DuckDb => "duckdb",
            Self::Redshift => "redshift",
            Self::Trino => "trino",
            Self::Spark => "spark",
            Self::ClickHouse => "clickhouse",
            Self::SQLite => "sqlite",
            Self::MsSql => "mssql",
        }
    }

    pub(crate) fn to_sqlparser_dialect(self) -> Box<dyn dialect::Dialect> {
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

impl fmt::Display for Dialect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Dialect {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "generic" => Ok(Self::Generic),
            "ansi" => Ok(Self::Ansi),
            "postgres" | "postgresql" => Ok(Self::PostgreSql),
            "mysql" => Ok(Self::MySql),
            "hive" => Ok(Self::Hive),
            "databricks" => Ok(Self::Databricks),
            "snowflake" => Ok(Self::Snowflake),
            "bigquery" => Ok(Self::BigQuery),
            "duckdb" => Ok(Self::DuckDb),
            "redshift" => Ok(Self::Redshift),
            "trino" => Ok(Self::Trino),
            "spark" | "sparksql" => Ok(Self::Spark),
            "clickhouse" => Ok(Self::ClickHouse),
            "sqlite" => Ok(Self::SQLite),
            "mssql" | "tsql" | "sqlserver" => Ok(Self::MsSql),
            _ => Err(format!("unknown SQL dialect: {value}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialect_names_round_trip_and_aliases_parse() {
        for dialect in Dialect::ALL {
            assert_eq!(dialect.name().parse::<Dialect>().unwrap(), dialect);
            assert_eq!(dialect.to_string(), dialect.name());
        }
        assert_eq!("postgres".parse(), Ok(Dialect::PostgreSql));
        assert_eq!("sqlserver".parse(), Ok(Dialect::MsSql));
        assert_eq!("sparksql".parse(), Ok(Dialect::Spark));
    }
}
