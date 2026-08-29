use anyhow::Result;

use super::shared::*;
use crate::cli::{self, DebugCommand, DebugOutputFormat, DialectArg};
use dlin_core::graph;
use dlin_core::graph::column_lineage::CatalogSnapshot;

fn read_sql_input(sql: Option<&str>) -> Result<String> {
    if let Some(s) = sql {
        return Ok(s.to_string());
    }
    // Read from stdin
    let mut stdin = std::io::stdin();
    if std::io::IsTerminal::is_terminal(&stdin) {
        anyhow::bail!("provide SQL as an argument or via stdin");
    }
    let mut buf = String::new();
    std::io::Read::read_to_string(&mut stdin, &mut buf)?;
    if buf.is_empty() {
        anyhow::bail!("no SQL input received from stdin");
    }
    Ok(buf)
}

/// Run the `debug` subcommand
#[cfg(not(tarpaulin_include))]
pub(crate) fn run_debug_command(args: cli::DebugArgs) -> Result<()> {
    match args.command {
        DebugCommand::ParseSql(args) => run_debug_parse_sql(args),
        DebugCommand::TraceColumn(args) => run_debug_trace_column(args),
    }
}

fn resolve_debug_dialect(argument: &DialectArg) -> Result<ResolvedDialect> {
    let resolved = classify_dialect(&argument.requested)?;
    if let Some(warning) = &resolved.warning {
        dlin_core::warn!("{}", warning);
    }
    Ok(resolved)
}

/// Run `debug parse-sql`
#[cfg(not(tarpaulin_include))]
fn run_debug_parse_sql(args: cli::DebugParseSqlArgs) -> Result<()> {
    use std::io::Write;

    let sql = read_sql_input(args.sql.as_deref())?;
    let resolved_dialect = resolve_debug_dialect(&args.dialect)?;
    let dialect = resolved_dialect.dialect;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match args.format {
        DebugOutputFormat::Ast => {
            let text = graph::column_lineage::debug_parse_sql_ast_debug(&sql, dialect)
                .map_err(|e| anyhow::anyhow!(e))?;
            writeln!(out, "{}", text)?;
        }
        DebugOutputFormat::Json => {
            let pretty = std::io::IsTerminal::is_terminal(&stdout);
            let text = graph::column_lineage::debug_parse_sql_json(&sql, dialect, pretty)
                .map_err(|e| anyhow::anyhow!(e))?;
            if let Err(e) = writeln!(out, "{}", text)
                && e.kind() != std::io::ErrorKind::BrokenPipe
            {
                return Err(e.into());
            }
        }
    }
    Ok(())
}

/// Parse a schema string like "table1:col1,col2;table2:col3,col4" into a `CatalogSnapshot`.
fn parse_schema_string(schema_str: &str) -> Result<CatalogSnapshot> {
    let mut schema = CatalogSnapshot::new();
    for table_def in schema_str.split(';') {
        let table_def = table_def.trim();
        if table_def.is_empty() {
            continue;
        }
        let (table_name, cols_str) = table_def.split_once(':').ok_or_else(|| {
            anyhow::anyhow!(
                "invalid schema format '{}': expected table:col1,col2",
                table_def
            )
        })?;
        let columns: Vec<String> = cols_str
            .split(',')
            .map(|c| c.trim())
            .filter(|c| !c.is_empty())
            .map(|c| c.to_string())
            .collect();
        if columns.is_empty() {
            anyhow::bail!(
                "invalid schema format '{}': table has no columns",
                table_name.trim()
            );
        }
        schema.add_table(table_name.trim(), columns);
    }
    Ok(schema)
}

/// Run `debug trace-column`
#[cfg(not(tarpaulin_include))]
fn run_debug_trace_column(args: cli::DebugTraceColumnArgs) -> Result<()> {
    use std::io::Write;

    let sql = read_sql_input(args.sql.as_deref())?;
    let resolved_dialect = resolve_debug_dialect(&args.dialect)?;
    let dialect = resolved_dialect.dialect;
    // Check that the SQL parses before validating `--schema`, so a bad SQL
    // string is reported even when `--schema` is also malformed — matching
    // the order these two inputs have always been evaluated in.
    graph::column_lineage::check_sql_parses(&sql, dialect)
        .map_err(|e| anyhow::anyhow!("parse error: {}", e))?;

    let catalog = args
        .schema
        .as_deref()
        .map(parse_schema_string)
        .transpose()?;

    let stdout = std::io::stdout();
    let pretty = std::io::IsTerminal::is_terminal(&stdout);
    let text = graph::column_lineage::debug_trace_column_json(
        &sql,
        dialect,
        catalog.as_ref(),
        &args.column,
        pretty,
    )
    .map_err(|e| anyhow::anyhow!(e))?;

    let mut out = stdout.lock();
    if let Err(e) = writeln!(out, "{}", text)
        && e.kind() != std::io::ErrorKind::BrokenPipe
    {
        return Err(e.into());
    }

    Ok(())
}
