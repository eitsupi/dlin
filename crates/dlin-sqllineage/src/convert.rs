use std::collections::{BTreeMap, HashMap};

use sqlparser::ast::{Query, SelectFlavor, SetExpr, Statement, Visit, Visitor};
use std::ops::ControlFlow;

use super::metadata::StatementMetadata;
use super::{
    Catalog, CatalogEntry, Diagnostic, DiagnosticKind, Dialect, Ident, Lineage, Location, Output,
    OutputColumn, OutputName, Relation, Severity, Shape, SourceColumn, StatementAnalysis,
    StatementKind, Transform, UnexpandedStar, Unresolved,
};
use crate::build::LineageBuilder;
use crate::graph::{
    RawGraph,
    node::RawNode,
    scope::{Binding, ScopeTree},
};
use crate::keys::{
    RelationKey, candidate_key, relation_key, sort_dedup_by_key, source_key, unresolved_key,
};
use crate::types::{self as old, CatalogProvider, SemanticError};

/// TODO: Remove this bridge when the resolver uses `Catalog` directly.
pub(super) struct CatalogAdapter<'a> {
    catalog: &'a dyn Catalog,
    dialect: Dialect,
}

impl<'a> CatalogAdapter<'a> {
    pub(super) fn new(catalog: &'a dyn Catalog, dialect: Dialect) -> Self {
        Self { catalog, dialect }
    }

    fn entry(&self, table: &old::TableRef) -> Option<CatalogEntry<'_>> {
        self.catalog.lookup(&plain_relation(table))
    }
}

impl CatalogProvider for CatalogAdapter<'_> {
    fn list_columns(&self, table: &old::TableRef) -> Option<Vec<Ident>> {
        Some(
            self.entry(table)?
                .columns
                .iter()
                .map(|col| col.name.clone())
                .collect(),
        )
    }

    fn resolve_column(
        &self,
        column: &Ident,
        candidates: &[old::TableRef],
    ) -> Option<old::TableRef> {
        let mut matching = candidates.iter().filter(|table| {
            self.entry(table).is_some_and(|entry| {
                entry
                    .columns
                    .iter()
                    .any(|col| col.name.column_key(self.dialect) == column.column_key(self.dialect))
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
    let metadata = StatementMetadata::new(statement, dialect);
    let provider = catalog.map(|adapter| adapter as &dyn CatalogProvider);
    let builder = LineageBuilder::new(dialect);
    let empty = empty_projection_diagnostic(statement, dialect);
    let (graph, warnings, kind) = match builder.build(statement) {
        Ok(result) => result,
        Err(error) => {
            return semantic_failure(
                statement_kind(statement),
                None,
                Vec::new(),
                &error,
                empty.into_iter().collect(),
                dialect,
            );
        }
    };
    let read_order: Vec<_> = graph
        .tables
        .inputs
        .iter()
        .flat_map(|table| canonical_relations(table, catalog))
        .collect();
    let row_values = row_value_outputs(&graph);
    let written = graph
        .tables
        .output
        .as_ref()
        .map(StatementMetadata::relation);
    let read = graph
        .tables
        .inputs
        .iter()
        .map(StatementMetadata::relation)
        .collect();
    let result = match crate::resolve::resolve(graph, provider, warnings, kind.clone()) {
        Ok(result) => result,
        Err(error) => {
            return semantic_failure(
                convert_kind(&kind),
                written,
                read,
                &error,
                empty.into_iter().collect(),
                dialect,
            );
        }
    };
    let mut converted = convert_result(
        &result,
        &metadata,
        catalog,
        &row_values,
        dialect,
        &read_order,
    );
    if let Some(diagnostic) = empty {
        if diagnostic.severity == Severity::Error {
            converted.output.columns.clear();
        }
        converted.diagnostics.push(diagnostic);
    }
    converted
}

// TODO: Rebuilds the new types from legacy results, including the `"*"` and
// `"?column?"` sentinel strings. Remove this when the resolver produces
// `StatementAnalysis` directly.
fn convert_result(
    result: &old::AnalyzeResult,
    metadata: &StatementMetadata,
    catalog: Option<&CatalogAdapter<'_>>,
    row_values: &BTreeMap<Ident, Vec<Ident>>,
    dialect: Dialect,
    read_order: &[Relation],
) -> StatementAnalysis {
    let source_order: Vec<_> = result
        .source_order
        .iter()
        .filter_map(|origin| {
            if let old::ColumnOrigin::Concrete { table, column } = origin {
                Some(SourceColumn {
                    relation: catalog
                        .and_then(|adapter| adapter.entry(table))
                        .map_or_else(|| plain_relation(table), |entry| entry.relation),
                    column: column.clone(),
                })
            } else {
                None
            }
        })
        .collect();
    let reference_positions = first_references(&source_order, dialect);
    let mut columns = Vec::new();
    let mut unexpanded = Vec::new();
    let mut position = Some(0);
    for (index, mapping) in result.columns.mappings.iter().enumerate() {
        let row_value = metadata.row_value_alias(index, &mapping.target.column);
        let name =
            metadata.output_name_at(index, result.columns.mappings.len(), &mapping.target.column);
        let is_star = mapping.target.column.value == "*"
            && mapping.target.column.quote.is_none()
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
            let lineage =
                normalize_lineage(known, unresolved, dialect, read_order, &reference_positions);
            let mut relations = Vec::new();
            star_relations(&mapping.sources, &mut relations);
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
            row_aliases.retain(|name| name.column_key(dialect) == alias.column_key(dialect));
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
        let lineage =
            normalize_lineage(known, unresolved, dialect, read_order, &reference_positions);
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
                .entry(id.column_key(dialect).into_owned())
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
    let mut read = read_order.to_vec();
    sort_dedup_by_key(&mut read, |relation| relation_key(relation, dialect));
    StatementAnalysis {
        kind: convert_kind(&result.statement_type),
        written: result
            .tables
            .output
            .as_ref()
            .map(StatementMetadata::relation),
        read,
        output: Output { columns, shape },
        has_unresolved_stars: result.columns.has_unresolved_stars,
        diagnostics,
    }
}

fn canonical_relations(
    table: &old::TableRef,
    catalog: Option<&CatalogAdapter<'_>>,
) -> Vec<Relation> {
    let relation = StatementMetadata::relation(table);
    vec![
        catalog
            .and_then(|adapter| adapter.catalog.lookup(&relation))
            .map_or(relation, |entry| entry.relation),
    ]
}

/// Position of the first resolved reference to each column, keyed by the
/// relation's dialect key and the column's exact spelling.
type FirstReferences = HashMap<(RelationKey, Ident), usize>;

fn first_references(source_order: &[SourceColumn], dialect: Dialect) -> FirstReferences {
    let mut positions = HashMap::with_capacity(source_order.len());
    for (index, reference) in source_order.iter().enumerate() {
        positions
            .entry((
                relation_key(&reference.relation, dialect),
                reference.column.clone(),
            ))
            .or_insert(index);
    }
    positions
}

fn normalize_lineage(
    mut known: Vec<SourceColumn>,
    unresolved: Vec<Unresolved>,
    dialect: Dialect,
    read_order: &[Relation],
    first_references: &FirstReferences,
) -> Lineage {
    // Relation spelling follows FROM order; column spelling follows the first
    // resolved reference in the statement, independently of which alias
    // supplied that reference.
    for source in &mut known {
        retain_first_relation_spelling(&mut source.relation, read_order, dialect);
    }
    known.sort_by_cached_key(|source| {
        (
            source_key(source, dialect),
            first_references
                .get(&(
                    relation_key(&source.relation, dialect),
                    source.column.clone(),
                ))
                .copied()
                .unwrap_or(usize::MAX),
        )
    });
    known.dedup_by(|later, earlier| source_key(later, dialect) == source_key(earlier, dialect));
    let mut unique = Vec::new();
    for mut item in unresolved {
        normalize_unresolved(&mut item, read_order, dialect);
        if !unique
            .iter()
            .any(|seen| unresolved_key(seen, dialect) == unresolved_key(&item, dialect))
        {
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

fn retain_first_relation_spelling(
    relation: &mut Relation,
    read_order: &[Relation],
    dialect: Dialect,
) {
    if let Some(first) = read_order
        .iter()
        .find(|first| relation_key(first, dialect) == relation_key(relation, dialect))
    {
        relation.clone_from(first);
    }
}

fn normalize_unresolved(unresolved: &mut Unresolved, read_order: &[Relation], dialect: Dialect) {
    match unresolved {
        Unresolved::BehindStar { relation } | Unresolved::NamedBehindStar { relation, .. } => {
            retain_first_relation_spelling(relation, read_order, dialect);
        }
        Unresolved::Ambiguous { candidates, .. } => {
            for candidate in candidates.iter_mut() {
                if let crate::Candidate::Relation(relation) = candidate {
                    retain_first_relation_spelling(relation, read_order, dialect);
                }
            }
            sort_dedup_by_key(candidates, |candidate| candidate_key(candidate, dialect));
        }
        _ => {}
    }
}

fn lift_origin(
    origin: &old::ColumnOrigin,
    output_index: usize,
    metadata: &StatementMetadata,
    catalog: Option<&CatalogAdapter<'_>>,
    row_aliases: &[Ident],
    known: &mut Vec<SourceColumn>,
    unresolved: &mut Vec<Unresolved>,
) {
    match origin {
        old::ColumnOrigin::Concrete { table, column } => {
            let relation = plain_relation(table);
            let relation = catalog
                .and_then(|adapter| adapter.entry(table))
                .map_or(relation, |entry| entry.relation);
            known.push(SourceColumn {
                relation,
                column: column.clone(),
            });
        }
        old::ColumnOrigin::Ambiguous { column, candidates } => {
            let column_ident = column.clone();
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
                sort_dedup_by_key(&mut candidates, |candidate| {
                    candidate_key(candidate, metadata.dialect())
                });
                unresolved.push(Unresolved::Ambiguous {
                    column: column_ident,
                    candidates,
                });
            }
        }
        old::ColumnOrigin::Wildcard { table } => unresolved.push(Unresolved::BehindStar {
            relation: StatementMetadata::relation(table),
        }),
        old::ColumnOrigin::NamedWildcard { table, column } => {
            unresolved.push(Unresolved::NamedBehindStar {
                relation: StatementMetadata::relation(table),
                column: column.clone(),
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
        old::ColumnOrigin::Ambiguous { column, .. } => column.value == "*" && column.quote.is_none(),
        old::ColumnOrigin::Recursive { base_sources } => base_sources.iter().any(|source| {
            matches!(source, old::ColumnOrigin::Wildcard { .. })
                || matches!(source, old::ColumnOrigin::Ambiguous { column, .. } if column.value == "*" && column.quote.is_none())
        }),
        _ => false,
    })
}

fn star_relations(origins: &[old::ColumnOrigin], result: &mut Vec<Option<Relation>>) {
    for origin in origins {
        match origin {
            old::ColumnOrigin::Wildcard { table } => {
                result.push(Some(StatementMetadata::relation(table)));
            }
            old::ColumnOrigin::Recursive { base_sources } => {
                star_relations(base_sources, result);
            }
            _ => {}
        }
    }
}

fn row_value_outputs(graph: &RawGraph) -> BTreeMap<Ident, Vec<Ident>> {
    fn visit(node: usize, graph: &RawGraph, seen: &mut Vec<usize>, aliases: &mut Vec<Ident>) {
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
                    if col.name.column_key(graph.dialect) == name.column_key(graph.dialect) {
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
    error: &SemanticError,
    mut diagnostics: Vec<Diagnostic>,
    dialect: Dialect,
) -> StatementAnalysis {
    let diagnostic = match error {
        SemanticError::SetOperationArityMismatch { left, right } => {
            DiagnosticKind::SetOperationArityMismatch {
                left: *left,
                right: *right,
            }
        }
        SemanticError::PipeOperators => DiagnosticKind::UnsupportedSyntax {
            what: "pipe operators",
        },
        SemanticError::UnpivotExpression => DiagnosticKind::UnsupportedSyntax {
            what: "UNPIVOT on an expression",
        },
    };
    sort_dedup_by_key(&mut read, |relation| relation_key(relation, dialect));
    diagnostics.insert(
        0,
        Diagnostic {
            severity: Severity::Error,
            kind: diagnostic,
            location: None,
        },
    );
    StatementAnalysis {
        kind,
        written,
        read,
        output: Output {
            columns: Vec::new(),
            shape: Shape::Closed,
        },
        has_unresolved_stars: false,
        diagnostics,
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
        catalog: table.catalog.clone(),
        schema: table.schema.clone(),
        name: table.table.clone(),
    }
}

/// Report a query whose projection list is empty.
///
/// A bare `FROM t` (as in `DuckDB` and `ClickHouse`) is read as `SELECT *`, so it
/// only warns. Any other empty list is an error, except in dialects whose
/// parsers accept it as a harmless `SELECT` with nothing selected.
fn empty_projection_diagnostic(statement: &Statement, dialect: Dialect) -> Option<Diagnostic> {
    #[derive(Default)]
    struct Empty {
        bare_select: bool,
        bare_from: bool,
    }
    impl Visitor for Empty {
        type Break = ();
        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
            fn walk(body: &SetExpr, found: &mut Empty) {
                match body {
                    SetExpr::Select(select) if select.projection.is_empty() => {
                        if select.flavor == SelectFlavor::FromFirstNoSelect {
                            found.bare_from = true;
                        } else {
                            found.bare_select = true;
                        }
                    }
                    SetExpr::SetOperation { left, right, .. } => {
                        walk(left, found);
                        walk(right, found);
                    }
                    _ => {}
                }
            }
            walk(&query.body, self);
            ControlFlow::Continue(())
        }
    }
    let mut found = Empty::default();
    let _ = statement.visit(&mut found);
    let severity =
        if found.bare_select && !matches!(dialect, Dialect::Generic | Dialect::PostgreSql) {
            Severity::Error
        } else if found.bare_select || found.bare_from {
            Severity::Warning
        } else {
            return None;
        };
    Some(Diagnostic {
        severity,
        kind: DiagnosticKind::EmptyProjection,
        location: None,
    })
}
