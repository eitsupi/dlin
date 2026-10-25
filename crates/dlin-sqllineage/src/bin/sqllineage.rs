use std::process;

use clap::Parser;
use sqllineage::{AnalyzeOptions, AnalyzeResult, ColumnOrigin, Dialect, TransformKind, analyze};

#[derive(Parser)]
#[command(
    name = "sqllineage",
    version,
    about = "Extract table and column-level lineage from SQL"
)]
struct Cli {
    /// SQL string to analyze
    #[arg(short = 'e', long = "execute")]
    sql: String,

    /// SQL dialect
    #[arg(short, long, default_value = "generic")]
    dialect: String,

    /// Output format: json, table, dot
    #[arg(short, long, default_value = "json")]
    format: String,

    /// Include column-level lineage
    #[arg(long)]
    columns: bool,
}

fn main() {
    let cli = Cli::parse();

    let dialect = match parse_dialect(&cli.dialect) {
        Some(d) => d,
        None => {
            eprintln!(
                "error: unknown dialect '{}'. valid: generic, ansi, postgresql, mysql, hive, databricks, snowflake, bigquery, duckdb, redshift, trino, spark, clickhouse, sqlite, mssql/tsql",
                cli.dialect
            );
            process::exit(1);
        }
    };

    let results = match analyze(
        &cli.sql,
        AnalyzeOptions {
            dialect,
            ..AnalyzeOptions::default()
        },
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };

    for (i, result) in results.iter().enumerate() {
        if i > 0 {
            println!();
        }
        let output = match cli.format.as_str() {
            "json" => format_json(result, cli.columns),
            "table" => format_table(result, cli.columns),
            "dot" => format_dot(result, cli.columns),
            other => {
                eprintln!("error: unknown format '{other}'. valid: json, table, dot");
                process::exit(1);
            }
        };
        println!("{output}");
    }
}

fn parse_dialect(s: &str) -> Option<Dialect> {
    match s.to_lowercase().as_str() {
        "generic" => Some(Dialect::Generic),
        "ansi" => Some(Dialect::Ansi),
        "postgresql" | "postgres" => Some(Dialect::PostgreSql),
        "mysql" => Some(Dialect::MySql),
        "hive" => Some(Dialect::Hive),
        "databricks" => Some(Dialect::Databricks),
        "snowflake" => Some(Dialect::Snowflake),
        "bigquery" => Some(Dialect::BigQuery),
        "duckdb" | "duck_db" => Some(Dialect::DuckDb),
        "redshift" => Some(Dialect::Redshift),
        "trino" => Some(Dialect::Trino),
        "spark" | "spark2" | "sparksql" => Some(Dialect::Spark),
        "clickhouse" | "click_house" => Some(Dialect::ClickHouse),
        "sqlite" => Some(Dialect::SQLite),
        "mssql" | "ms_sql" | "tsql" | "t-sql" | "sqlserver" => Some(Dialect::MsSql),
        _ => None,
    }
}

fn format_json(result: &AnalyzeResult, columns: bool) -> String {
    if columns {
        serde_json::to_string_pretty(result).unwrap_or_default()
    } else {
        let val = serde_json::json!({
            "statement_type": result.statement_type,
            "tables": {
                "inputs": result.tables.inputs.iter().map(|t| t.to_string()).collect::<Vec<_>>(),
                "output": result.tables.output.as_ref().map(|t| t.to_string()),
            },
            "warnings": result.warnings,
        });
        serde_json::to_string_pretty(&val).unwrap_or_default()
    }
}

fn format_table(result: &AnalyzeResult, columns: bool) -> String {
    let mut out = String::new();

    out.push_str(&format!("Statement: {:?}\n", result.statement_type));

    let inputs: Vec<String> = result.tables.inputs.iter().map(|t| t.to_string()).collect();
    out.push_str(&format!(
        "Tables:\n  inputs:  {}\n",
        if inputs.is_empty() {
            "(none)".to_string()
        } else {
            inputs.join(", ")
        }
    ));
    out.push_str(&format!(
        "  output:  {}\n",
        result
            .tables
            .output
            .as_ref()
            .map(|t| t.to_string())
            .unwrap_or_else(|| "(none)".to_string())
    ));

    if columns && !result.columns.mappings.is_empty() {
        out.push_str("\nColumns:\n");
        for m in &result.columns.mappings {
            let target = format_colref(&m.target.table, &m.target.column);
            let sources: Vec<String> = m.sources.iter().map(format_origin).collect();
            let transform = transform_str(&m.transform);
            out.push_str(&format!(
                "  {target}  \u{2190}  {}    ({transform})\n",
                sources.join(", ")
            ));
        }
    }

    if !result.warnings.is_empty() {
        out.push_str(&format!("\nWarnings: {}\n", result.warnings.len()));
    }

    out.trim_end().to_string()
}

fn format_colref(table: &Option<sqllineage::TableRef>, column: &str) -> String {
    match table {
        Some(t) => format!("{t}.{column}"),
        None => column.to_string(),
    }
}

fn format_origin(origin: &ColumnOrigin) -> String {
    match origin {
        ColumnOrigin::Concrete { table, column } => format!("{table}.{column}"),
        ColumnOrigin::Ambiguous { column, .. } => format!("?{column}?"),
        ColumnOrigin::Wildcard { table } => format!("{table}.*"),
        ColumnOrigin::NamedWildcard { table, column } => format!("{table}.*({column})"),
        ColumnOrigin::SourceFree { column } => format!("<literal:{column}>"),
        ColumnOrigin::Recursive { base_sources } => {
            let inner: Vec<String> = base_sources.iter().map(format_origin).collect();
            format!("recursive({})", inner.join(", "))
        }
        _ => "<unknown>".to_string(),
    }
}

fn transform_str(kind: &TransformKind) -> &'static str {
    match kind {
        TransformKind::Direct => "direct",
        TransformKind::Expression => "expression",
        TransformKind::Aggregation => "aggregation",
        TransformKind::Conditional => "conditional",
        TransformKind::Window => "window",
        TransformKind::Unknown => "unknown",
    }
}

fn format_dot(result: &AnalyzeResult, columns: bool) -> String {
    let mut out = String::from("digraph lineage {\n  rankdir=LR;\n");

    if let Some(ref output) = result.tables.output {
        for input in &result.tables.inputs {
            out.push_str(&format!("  \"{input}\" -> \"{output}\";\n"));
        }
    }

    if columns {
        for m in &result.columns.mappings {
            let target = format_colref(&m.target.table, &m.target.column);
            for source in &m.sources {
                let src = format_origin(source);
                let label = transform_str(&m.transform);
                if label == "direct" {
                    out.push_str(&format!("  \"{src}\" -> \"{target}\";\n"));
                } else {
                    out.push_str(&format!(
                        "  \"{src}\" -> \"{target}\" [label=\"{label}\"];\n"
                    ));
                }
            }
        }
    }

    out.push('}');
    out
}

#[cfg(test)]
mod tests {
    use super::parse_dialect;
    use sqllineage::Dialect;

    #[test]
    fn parses_added_dialects_and_tsql_aliases() {
        assert!(matches!(parse_dialect("duckdb"), Some(Dialect::DuckDb)));
        assert!(matches!(parse_dialect("redshift"), Some(Dialect::Redshift)));
        assert!(matches!(parse_dialect("trino"), Some(Dialect::Trino)));
        assert!(matches!(parse_dialect("spark"), Some(Dialect::Spark)));
        assert!(matches!(
            parse_dialect("clickhouse"),
            Some(Dialect::ClickHouse)
        ));
        assert!(matches!(parse_dialect("sqlite"), Some(Dialect::SQLite)));
        assert!(matches!(parse_dialect("tsql"), Some(Dialect::MsSql)));
        assert!(parse_dialect("not-a-dialect").is_none());
    }
}
