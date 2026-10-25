use std::collections::BTreeMap;

use sqlparser::ast::{Query, SetExpr, Statement, Visit, Visitor};
use std::ops::ControlFlow;

use super::metadata::StatementMetadata;
use super::{
    Candidate, Catalog, CatalogEntry, Diagnostic, DiagnosticKind, Dialect, Ident, Lineage,
    Location, Output, OutputColumn, OutputName, Relation, Severity, Shape, SourceColumn,
    StatementAnalysis, StatementKind, Transform, UnexpandedStar, Unresolved,
};
use crate::build::LineageBuilder;
use crate::graph::{
    RawGraph,
    node::RawNode,
    scope::{Binding, ScopeTree},
};
use crate::types::{self as old, CatalogProvider};

pub(super) struct CatalogAdapter<'a> {
    catalog: &'a dyn Catalog,
    dialect: Dialect,
    metadata: Option<&'a StatementMetadata>,
}

impl<'a> CatalogAdapter<'a> {
    pub(super) fn new(catalog: &'a dyn Catalog, dialect: Dialect) -> Self {
        Self {
            catalog,
            dialect,
            metadata: None,
        }
    }

    fn relation(&self, table: &old::TableRef) -> Relation {
        self.metadata
            .map_or_else(|| plain_relation(table), |meta| meta.relation(table))
    }

    fn entry(&self, table: &old::TableRef) -> Option<CatalogEntry<'_>> {
        if self
            .metadata
            .is_some_and(|meta| meta.relation_is_ambiguous(table))
        {
            return None;
        }
        self.catalog.lookup(&self.relation(table))
    }
}

impl CatalogProvider for CatalogAdapter<'_> {
    fn list_columns(&self, table: &old::TableRef) -> Option<Vec<String>> {
        Some(
            self.entry(table)?
                .columns
                .iter()
                .map(|col| col.name.value.clone())
                .collect(),
        )
    }

    fn resolve_column(&self, column: &str, candidates: &[old::TableRef]) -> Option<old::TableRef> {
        let ident = if let Some(metadata) = self.metadata {
            metadata.lookup_ident(column)?
        } else {
            Ident::unquoted(column)
        };
        let mut matching = candidates.iter().filter(|table| {
            self.entry(table).is_some_and(|entry| {
                entry.columns.iter().any(|col| {
                    col.name.key(self.dialect) == ident.key(self.dialect)
                        || (matches!(self.dialect, Dialect::Generic | Dialect::BigQuery)
                            && ident.quote.is_none()
                            && col.name.value.to_lowercase() == ident.value.to_lowercase())
                })
            })
        });
        let only = matching.next()?;
        matching.next().is_none().then(|| only.clone())
    }
}

pub(super) fn analyze_statement(
    statement: &Statement,
    dialect: Dialect,
    catalog: Option<&CatalogAdapter<'_>>,
) -> StatementAnalysis {
    let metadata = StatementMetadata::new(statement);
    let adapter = catalog.map(|adapter| CatalogAdapter {
        catalog: adapter.catalog,
        dialect,
        metadata: Some(&metadata),
    });
    let provider = adapter.as_ref().map(|a| a as &dyn CatalogProvider);
    let builder = LineageBuilder::new(true, dialect);
    let (graph, warnings, kind) = match builder.build(statement) {
        Ok(result) => result,
        Err(error) => {
            return semantic_failure(statement_kind(statement), None, Vec::new(), &error.message);
        }
    };
    let row_values = row_value_outputs(&graph);
    let written = graph
        .tables
        .output
        .as_ref()
        .map(|table| metadata.written_relation(table));
    let read = graph
        .tables
        .inputs
        .iter()
        .map(|table| metadata.relation(table))
        .collect();
    let result = match crate::resolve::resolve(graph, provider, warnings, kind.clone()) {
        Ok(result) => result,
        Err(error) => return semantic_failure(convert_kind(&kind), written, read, &error.message),
    };
    let mut converted = convert_result(&result, &metadata, adapter.as_ref(), &row_values, dialect);
    let mut empty = EmptyProjection(false);
    let _ = statement.visit(&mut empty);
    if empty.0 {
        let severity = if matches!(dialect, Dialect::Generic | Dialect::PostgreSql) {
            Severity::Warning
        } else {
            Severity::Error
        };
        if severity == Severity::Error {
            converted.output.columns.clear();
        }
        converted.diagnostics.push(Diagnostic {
            severity,
            kind: DiagnosticKind::EmptyProjection,
            location: None,
        });
    }
    converted
}

fn convert_result(
    result: &old::AnalyzeResult,
    metadata: &StatementMetadata,
    catalog: Option<&CatalogAdapter<'_>>,
    row_values: &BTreeMap<String, Vec<String>>,
    dialect: Dialect,
) -> StatementAnalysis {
    let mut columns = Vec::new();
    let mut unexpanded = Vec::new();
    let mut position = Some(0);
    for (index, mapping) in result.columns.mappings.iter().enumerate() {
        let row_value = metadata.row_value_alias(index, &mapping.target.column);
        let mut name =
            metadata.output_name_at(index, result.columns.mappings.len(), &mapping.target.column);
        let is_star = mapping.target.column == "*"
            && is_star_mapping(mapping)
            && !matches!(&name, OutputName::Explicit(id) if id.value == "*" && id.quote.is_some());
        if is_star {
            let mut known = Vec::new();
            let mut unresolved = Vec::new();
            for origin in &mapping.sources {
                lift_origin(
                    origin,
                    index,
                    metadata,
                    catalog,
                    &[],
                    &mut known,
                    &mut unresolved,
                );
            }
            for item in &mut unresolved {
                if matches!(item, Unresolved::Unbound { column } if column.value == "*" && column.quote.is_none())
                {
                    *item = Unresolved::Unsupported {
                        what: "star expansion",
                    };
                }
            }
            let lineage = normalize_lineage(known, unresolved);
            let mut relations = Vec::new();
            star_relations(&mapping.sources, metadata, &mut relations);
            if relations.is_empty() {
                relations.push(None);
            }
            for relation in relations {
                unexpanded.push(UnexpandedStar {
                    relation,
                    after_ordinal: columns
                        .last()
                        .and_then(|column: &OutputColumn| column.ordinal),
                    lineage: lineage.clone(),
                });
            }
            position = None;
            continue;
        }
        let mut known = Vec::new();
        let mut unresolved = Vec::new();
        let mut row_aliases = row_values
            .get(&mapping.target.column)
            .cloned()
            .unwrap_or_default();
        if let Some(alias) = row_value {
            row_aliases.retain(|name| *name == alias.value);
        }
        for origin in &mapping.sources {
            lift_origin(
                origin,
                index,
                metadata,
                catalog,
                &row_aliases,
                &mut known,
                &mut unresolved,
            );
        }
        known.sort();
        known.dedup();
        if metadata.is_star_expansion(index, result.columns.mappings.len())
            && let [source] = known.as_slice()
            && source.column.value == mapping.target.column
        {
            name = OutputName::Explicit(source.column.clone());
        }
        let lineage = normalize_lineage(known, unresolved);
        columns.push(OutputColumn {
            ordinal: position,
            name,
            lineage,
            transform: convert_transform(&mapping.transform),
        });
        position = position.map(|n| n + 1);
    }
    let mut diagnostics = result
        .warnings
        .iter()
        .map(|warning| Diagnostic {
            severity: Severity::Error,
            kind: match warning.kind {
                old::WarningKind::UnexpectedCycle => DiagnosticKind::UnexpectedCycle,
            },
            location: warning.location.as_ref().map(|loc| Location {
                line: loc.line,
                column: loc.column,
            }),
        })
        .collect::<Vec<_>>();
    let mut names: BTreeMap<String, (Ident, usize, Vec<usize>)> = BTreeMap::new();
    for column in &columns {
        if let OutputName::Explicit(id) | OutputName::Inferred(id) = &column.name {
            let group = names
                .entry(id.key(dialect).into_owned())
                .or_insert_with(|| (id.clone(), 0, Vec::new()));
            group.1 += 1;
            if let Some(ordinal) = column.ordinal {
                group.2.push(ordinal);
            }
        }
    }
    for (_, (name, count, ordinals)) in names {
        if count > 1 {
            diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                kind: DiagnosticKind::DuplicateOutputName { name, ordinals },
                location: None,
            });
        }
    }
    let shape = if unexpanded.is_empty() {
        Shape::Closed
    } else {
        Shape::Open { unexpanded }
    };
    let mut read: Vec<_> = result
        .tables
        .inputs
        .iter()
        .flat_map(|t| canonical_relations(t, metadata, catalog))
        .collect();
    read.sort();
    read.dedup();
    StatementAnalysis {
        kind: convert_kind(&result.statement_type),
        written: result
            .tables
            .output
            .as_ref()
            .map(|t| metadata.written_relation(t)),
        read,
        output: Output { columns, shape },
        has_unresolved_stars: result.columns.has_unresolved_stars,
        diagnostics,
    }
}

fn canonical_relations(
    table: &old::TableRef,
    metadata: &StatementMetadata,
    catalog: Option<&CatalogAdapter<'_>>,
) -> Vec<Relation> {
    let mut relations = metadata.relation_candidates(table);
    if relations.is_empty() {
        relations.push(metadata.relation(table));
    }
    relations
        .into_iter()
        .map(|relation| {
            catalog
                .and_then(|adapter| adapter.catalog.lookup(&relation))
                .map_or(relation, |entry| entry.relation)
        })
        .collect()
}

fn normalize_lineage(mut known: Vec<SourceColumn>, unresolved: Vec<Unresolved>) -> Lineage {
    known.sort();
    known.dedup();
    let mut unique = Vec::new();
    for item in unresolved {
        if !unique.contains(&item) {
            unique.push(item);
        }
    }
    if unique.is_empty() {
        Lineage::Complete(known)
    } else {
        Lineage::Partial {
            known,
            unresolved: unique,
        }
    }
}

fn lift_origin(
    origin: &old::ColumnOrigin,
    output_index: usize,
    metadata: &StatementMetadata,
    catalog: Option<&CatalogAdapter<'_>>,
    row_aliases: &[String],
    known: &mut Vec<SourceColumn>,
    unresolved: &mut Vec<Unresolved>,
) {
    match origin {
        old::ColumnOrigin::Concrete { table, column } => {
            let mut ident = metadata.source_ident_for_output(output_index, table, column);
            let context = metadata.relation_for_output(output_index, table, column);
            let alternatives = metadata.relation_candidates(table);
            if context.is_none() && alternatives.len() > 1 {
                unresolved.push(Unresolved::Ambiguous {
                    column: ident,
                    candidates: alternatives.into_iter().map(Candidate::Relation).collect(),
                });
                return;
            }
            if let Some(catalog) = catalog
                && let Some(entry) = context.as_ref().map_or_else(
                    || catalog.entry(table),
                    |relation| catalog.catalog.lookup(relation),
                )
            {
                if !metadata.has_column_reference(column)
                    && let Some(schema_column) =
                        entry.columns.iter().find(|c| c.name.value == *column)
                {
                    ident = schema_column.name.clone();
                }
                known.push(SourceColumn {
                    relation: entry.relation,
                    column: ident,
                });
            } else {
                known.push(SourceColumn {
                    relation: context.unwrap_or_else(|| metadata.relation(table)),
                    column: ident,
                });
            }
        }
        old::ColumnOrigin::Ambiguous { column, candidates } => {
            let column_ident = metadata.output_reference_ident(output_index, column);
            if candidates.is_empty() {
                if row_aliases.contains(column) {
                    unresolved.push(Unresolved::RowValue {
                        alias: column_ident,
                    });
                } else {
                    unresolved.push(Unresolved::Unbound {
                        column: column_ident,
                    });
                }
            } else {
                let mut candidates: Vec<_> = candidates
                    .iter()
                    .flat_map(|table| metadata.candidates_for_output(output_index, column, table))
                    .collect();
                candidates.sort();
                candidates.dedup();
                unresolved.push(Unresolved::Ambiguous {
                    column: column_ident,
                    candidates,
                });
            }
        }
        old::ColumnOrigin::Wildcard { table } => unresolved.push(Unresolved::BehindStar {
            relation: metadata.relation(table),
        }),
        old::ColumnOrigin::NamedWildcard { table, column } => {
            unresolved.push(Unresolved::NamedBehindStar {
                relation: metadata.relation(table),
                column: metadata.source_ident(table, column),
            });
        }
        old::ColumnOrigin::SourceFree { .. } => {}
        old::ColumnOrigin::Recursive { base_sources } => {
            for source in base_sources {
                lift_origin(
                    source,
                    output_index,
                    metadata,
                    catalog,
                    row_aliases,
                    known,
                    unresolved,
                );
            }
            unresolved.push(Unresolved::Recursive);
        }
    }
}

fn is_star_mapping(mapping: &old::ColumnMapping) -> bool {
    mapping.sources.iter().any(|origin| match origin {
        old::ColumnOrigin::Wildcard { .. } => true,
        old::ColumnOrigin::Ambiguous { column, .. } => column == "*",
        old::ColumnOrigin::Recursive { base_sources } => base_sources.iter().any(|source| {
            matches!(source, old::ColumnOrigin::Wildcard { .. })
                || matches!(source, old::ColumnOrigin::Ambiguous { column, .. } if column == "*")
        }),
        _ => false,
    })
}

fn star_relations(
    origins: &[old::ColumnOrigin],
    metadata: &StatementMetadata,
    result: &mut Vec<Option<Relation>>,
) {
    for origin in origins {
        match origin {
            old::ColumnOrigin::Wildcard { table } => result.push(Some(metadata.relation(table))),
            old::ColumnOrigin::Recursive { base_sources } => {
                star_relations(base_sources, metadata, result);
            }
            _ => {}
        }
    }
}

fn row_value_outputs(graph: &RawGraph) -> BTreeMap<String, Vec<String>> {
    fn visit(node: usize, graph: &RawGraph, seen: &mut Vec<usize>, aliases: &mut Vec<String>) {
        if seen.contains(&node) {
            return;
        }
        seen.push(node);
        match &graph.nodes[node] {
            RawNode::RowValueCandidate { name, .. } => aliases.push(name.clone()),
            RawNode::Ref {
                name,
                binding: Some(Binding::Cte(scope) | Binding::DerivedTable(scope)),
                ..
            } => {
                for col in graph.scopes.output_columns(*scope) {
                    if col.name == *name {
                        visit(col.node_id, graph, seen, aliases);
                    }
                }
            }
            _ => {}
        }
        for edge in &graph.edges {
            if edge.to == node {
                visit(edge.from, graph, seen, aliases);
            }
        }
    }
    let mut result = BTreeMap::new();
    for column in graph.scopes.output_columns(ScopeTree::root()) {
        let mut aliases = Vec::new();
        visit(column.node_id, graph, &mut Vec::new(), &mut aliases);
        result
            .entry(column.name.clone())
            .or_insert_with(Vec::new)
            .extend(aliases);
    }
    result
}

fn semantic_failure(
    kind: StatementKind,
    written: Option<Relation>,
    mut read: Vec<Relation>,
    message: &str,
) -> StatementAnalysis {
    let diagnostic =
        if let Some(arity) = message.strip_prefix("set operation arity mismatch: left has ") {
            let (left, right) = arity
                .split_once(" columns, right has ")
                .expect("engine arity format");
            DiagnosticKind::SetOperationArityMismatch {
                left: left.parse().expect("engine arity"),
                right: right
                    .trim_end_matches(" columns")
                    .parse()
                    .expect("engine arity"),
            }
        } else {
            DiagnosticKind::UnsupportedSyntax {
                what: if message.starts_with("pipe operators") {
                    "pipe operators"
                } else {
                    "UNPIVOT on an expression"
                },
            }
        };
    read.sort();
    read.dedup();
    StatementAnalysis {
        kind,
        written,
        read,
        output: Output {
            columns: Vec::new(),
            shape: Shape::Closed,
        },
        has_unresolved_stars: false,
        diagnostics: vec![Diagnostic {
            severity: Severity::Error,
            kind: diagnostic,
            location: None,
        }],
    }
}

fn statement_kind(statement: &Statement) -> StatementKind {
    match statement {
        Statement::Query(_) => StatementKind::Query,
        Statement::Insert(_) => StatementKind::Insert,
        Statement::CreateTable(create) if create.query.is_some() => StatementKind::CreateTableAs,
        Statement::Update(_) => StatementKind::Update,
        Statement::Delete(_) => StatementKind::Delete,
        Statement::Merge(_) => StatementKind::Merge,
        _ => StatementKind::Other,
    }
}

fn convert_kind(kind: &old::StatementType) -> StatementKind {
    match kind {
        old::StatementType::Query => StatementKind::Query,
        old::StatementType::Insert => StatementKind::Insert,
        old::StatementType::CreateTable => StatementKind::CreateTableAs,
        old::StatementType::Update => StatementKind::Update,
        old::StatementType::Delete => StatementKind::Delete,
        old::StatementType::Merge => StatementKind::Merge,
        old::StatementType::Other => StatementKind::Other,
    }
}

fn convert_transform(transform: &old::TransformKind) -> Transform {
    match transform {
        old::TransformKind::Direct => Transform::Direct,
        old::TransformKind::Expression => Transform::Expression,
        old::TransformKind::Aggregation => Transform::Aggregation,
        old::TransformKind::Conditional => Transform::Conditional,
        old::TransformKind::Window => Transform::Window,
        old::TransformKind::Unknown => Transform::Unknown,
    }
}

fn plain_relation(table: &old::TableRef) -> Relation {
    Relation {
        catalog: table.catalog.as_ref().map(Ident::unquoted),
        schema: table.schema.as_ref().map(Ident::unquoted),
        name: Ident::unquoted(&table.table),
    }
}

struct EmptyProjection(bool);
impl Visitor for EmptyProjection {
    type Break = ();
    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        fn is_empty(body: &SetExpr) -> bool {
            match body {
                SetExpr::Select(select) => {
                    select.projection.is_empty()
                        && select.flavor != sqlparser::ast::SelectFlavor::FromFirstNoSelect
                }
                SetExpr::SetOperation { left, right, .. } => is_empty(left) || is_empty(right),
                _ => false,
            }
        }
        self.0 |= is_empty(&query.body);
        ControlFlow::Continue(())
    }
}
