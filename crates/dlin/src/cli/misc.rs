use std::path::PathBuf;

use super::*;
use clap::Subcommand;

#[derive(Debug, clap::Args)]
pub struct McpArgs {
    /// Path to dbt project directory
    #[arg(short = 'p', long = "project-dir", default_value = ".")]
    pub project_dir: PathBuf,

    /// Path to manifest.json file or directory containing target/manifest.json
    #[arg(long)]
    pub manifest_path: Option<PathBuf>,

    /// SQL dialect for parsing compiled SQL.
    /// Auto-detected from manifest.metadata.adapter_type when omitted. Recognized dialects
    /// removed from the active backend fall back to Generic with a warning.
    #[arg(
        long,
        value_parser = parse_dialect_arg,
        long_help = "\
SQL dialect for parsing compiled SQL in get_column_lineage.

When omitted, the dialect is auto-detected from manifest.metadata.adapter_type.
Recognized dialects removed from the active backend fall back to Generic with a warning;
a missing, empty, or unknown adapter_type is an error."
    )]
    pub dialect: Option<DialectArg>,
}

#[derive(Debug, clap::Args)]
pub struct CheckManifestArgs {
    /// Path to dbt project directory
    #[arg(short = 'p', long = "project-dir", default_value = ".")]
    pub project_dir: PathBuf,

    /// Path to manifest.json file or directory containing target/manifest.json
    #[arg(
        long,
        long_help = "\
Path to manifest.json file or directory containing target/manifest.json.

Default: <project-dir>/target/manifest.json"
    )]
    pub manifest_path: Option<PathBuf>,

    /// Output format: text (default) or json
    #[arg(short = 'o', long, default_value = "text")]
    pub output: CheckManifestOutputFormat,

    /// Suppress warning messages (exit code only)
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

#[derive(Debug, clap::Args)]
pub struct DebugArgs {
    #[command(subcommand)]
    pub command: DebugCommand,
}

#[derive(Debug, Subcommand)]
pub enum DebugCommand {
    /// Parse SQL and display the AST (debug or JSON)
    #[command(
        name = "parse-sql",
        long_about = "\
Parse a SQL statement using the production SQL parser and display the result.

By default, shows the Rust Debug representation of the AST. \
Use --format to choose between AST debug output or JSON AST.

This does not require a dbt project — it operates on raw SQL strings.",
        after_long_help = "\
Examples:
  # Show AST debug representation (default)
  dlin debug parse-sql 'SELECT a, b FROM t'

  # Show AST as JSON
  dlin debug parse-sql 'SELECT a FROM t' --format json

  # Parse with BigQuery dialect
  dlin debug parse-sql 'SELECT CAST(x AS ARRAY<STRING>) FROM t' --dialect bigquery

  # Parse from file via stdin
  dlin debug parse-sql --dialect snowflake < compiled_query.sql"
    )]
    ParseSql(DebugParseSqlArgs),

    /// Trace a single column's lineage through a SQL statement
    #[command(
        name = "trace-column",
        long_about = "\
Trace a single column's upstream lineage through a SQL statement.

Uses the production sqllineage engine to find where a column comes from. \
Optionally provide table schema information for more accurate resolution \
(especially needed for SELECT * expansion).

This does not require a dbt project — it operates on raw SQL strings.",
        after_long_help = "\
Examples:
  # Basic column trace
  dlin debug trace-column 'SELECT t.id AS order_id FROM t' --column order_id

  # With schema (table:col1,col2 format, semicolon-separated tables)
  dlin debug trace-column \\
    'SELECT * FROM orders JOIN customers ON orders.cid = customers.id' \\
    --column cid \\
    --schema 'orders:id,cid,amount;customers:id,name'

  # With explicit dialect
  dlin debug trace-column 'SELECT a FROM t' --column a --dialect bigquery

  # From file via stdin
  dlin debug trace-column --column order_id --dialect snowflake < query.sql"
    )]
    TraceColumn(DebugTraceColumnArgs),
}

#[derive(Debug, clap::Args)]
pub struct DebugParseSqlArgs {
    /// SQL string to parse (reads from stdin if omitted)
    pub sql: Option<String>,

    /// SQL dialect for parsing (default: generic)
    /// Recognized dialects removed from the active backend fall back to Generic with a warning.
    #[arg(long, default_value = "generic", value_parser = parse_dialect_arg)]
    pub dialect: DialectArg,

    /// Output format: ast (Debug representation), json (JSON AST)
    #[arg(long, default_value = "ast")]
    pub format: DebugOutputFormat,
}

#[derive(Debug, clap::Args)]
pub struct DebugTraceColumnArgs {
    /// SQL string to parse (reads from stdin if omitted)
    pub sql: Option<String>,

    /// Column name to trace
    #[arg(long)]
    pub column: String,

    /// SQL dialect for parsing (default: generic)
    /// Recognized dialects removed from the active backend fall back to Generic with a warning.
    #[arg(long, default_value = "generic", value_parser = parse_dialect_arg)]
    pub dialect: DialectArg,

    /// Table schema definitions for accurate lineage resolution.
    /// Format: table1:col1,col2;table2:col3,col4
    #[arg(long)]
    pub schema: Option<String>,
}

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum DebugOutputFormat {
    /// Rust Debug representation of the AST
    Ast,
    /// JSON serialization of the AST
    Json,
}
