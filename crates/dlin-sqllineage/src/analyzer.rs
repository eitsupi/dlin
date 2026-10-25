use sqlparser::ast::Statement;
use sqlparser::parser::{Parser, ParserError};

use super::convert::{CatalogAdapter, analyze_statement};
use super::{Analysis, Catalog, Dialect, Location, Options, ParseError, ParseErrorKind};

/// A reusable SQL analyzer borrowing its optional catalog.
#[must_use]
pub struct Analyzer<'c> {
    pub(super) dialect: Dialect,
    pub(super) catalog: Option<&'c dyn Catalog>,
    options: Options,
}

impl<'c> Analyzer<'c> {
    /// Create an analyzer for a SQL dialect.
    pub fn new(dialect: Dialect) -> Self {
        Self {
            dialect,
            catalog: None,
            options: Options::default(),
        }
    }

    /// Borrow schema metadata for star expansion and column resolution.
    pub fn with_catalog(mut self, catalog: &'c dyn Catalog) -> Self {
        self.catalog = Some(catalog);
        self
    }

    /// Set parser options.
    pub fn with_options(mut self, options: Options) -> Self {
        self.options = options;
        self
    }

    /// Parse SQL and analyze every statement in its original order.
    ///
    /// Syntax errors fail the batch. Semantic errors are attached to the
    /// affected statement and leave its output columns empty.
    pub fn analyze(&self, sql: &str) -> Result<Analysis, ParseError> {
        let parsed = parse_with_options(sql, self.dialect, &self.options)?;
        Ok(parsed.analyze(self))
    }
}

/// Parsed SQL with its parser representation kept private.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ParsedSql {
    statements: Vec<Statement>,
}

impl ParsedSql {
    /// Return the number of parsed statements.
    pub fn statement_count(&self) -> usize {
        self.statements.len()
    }

    /// Format the parser's AST for debugging.
    pub fn ast_debug(&self) -> String {
        format!("{:#?}", self.statements)
    }

    /// Serialize the parser's AST for debugging.
    ///
    /// The JSON representation is not a stable contract.
    pub fn ast_json(&self, pretty: bool) -> String {
        if pretty {
            serde_json::to_string_pretty(&self.statements)
        } else {
            serde_json::to_string(&self.statements)
        }
        .expect("SQL AST serialization cannot fail")
    }

    /// Analyze the parsed statements without parsing them again.
    ///
    /// Parsing uses the dialect passed to [`parse`]; resolution and catalog
    /// matching use the analyzer's dialect.
    pub fn analyze(&self, analyzer: &Analyzer<'_>) -> Analysis {
        let adapter = analyzer
            .catalog
            .map(|catalog| CatalogAdapter::new(catalog, analyzer.dialect));
        Analysis {
            statements: self
                .statements
                .iter()
                .map(|statement| analyze_statement(statement, analyzer.dialect, adapter.as_ref()))
                .collect(),
        }
    }
}

/// Parse SQL without performing lineage analysis.
pub fn parse(sql: &str, dialect: Dialect) -> Result<ParsedSql, ParseError> {
    parse_with_options(sql, dialect, &Options::default())
}

fn parse_with_options(
    sql: &str,
    dialect: Dialect,
    options: &Options,
) -> Result<ParsedSql, ParseError> {
    let parser_dialect = dialect.to_sqlparser_dialect();
    let mut parser = Parser::new(&*parser_dialect);
    if let Some(limit) = options.max_recursion_depth {
        parser = parser.with_recursion_limit(limit);
    }
    parser
        .try_with_sql(sql)
        .and_then(|mut parser| parser.parse_statements())
        .map(|statements| ParsedSql { statements })
        .map_err(|error| parse_error(&error))
}

fn parse_error(error: &ParserError) -> ParseError {
    let kind = match &error {
        ParserError::TokenizerError(_) => ParseErrorKind::Tokenizer,
        ParserError::ParserError(_) => ParseErrorKind::Syntax,
        ParserError::RecursionLimitExceeded => ParseErrorKind::RecursionLimit,
    };
    let message = error.to_string();
    // sqlparser retains error locations only in the formatted message.
    let location = message.rsplit_once("Line: ").and_then(|(_, position)| {
        let (line, column) = position.split_once(", Column: ")?;
        Some(Location {
            line: line.trim().parse().ok()?,
            column: column.trim().parse().ok()?,
        })
    });
    ParseError {
        message,
        kind,
        location,
    }
}
