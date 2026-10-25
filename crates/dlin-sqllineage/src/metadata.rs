//! AST metadata used to restore source spelling at the public boundary.

use std::ops::ControlFlow;

use sqlparser::ast::{self, Expr, ObjectName, Query, Statement, TableFactor, Visit, Visitor};

use crate::types::TableRef;

mod projection;

use super::{Candidate, Ident, OutputName, Quote, Relation};
use projection::query_output;

/// Immutable spelling and output-shape data collected from one parsed statement.
pub(super) struct StatementMetadata {
    relations: Vec<Relation>,
    relation_aliases: Vec<(String, Relation)>,
    columns: Vec<ColumnOccurrence>,
    output: Vec<OutputSlot>,
    written: Option<Relation>,
    scope_candidates: Vec<Candidate>,
}

#[derive(Clone)]
struct ColumnOccurrence {
    ident: Ident,
    qualifier: Option<String>,
    owner: Option<Relation>,
    scope_candidates: Vec<Candidate>,
}

#[derive(Clone)]
struct OutputSlot {
    name: OutputName,
    is_star: bool,
    row_value: Option<Ident>,
    source_columns: Vec<ColumnOccurrence>,
}

impl StatementMetadata {
    pub(super) fn new(statement: &Statement) -> Self {
        let mut collector = AstMetadataCollector::default();
        let _ = statement.visit(&mut collector);
        Self {
            relations: collector.relations,
            relation_aliases: collector.relation_aliases,
            columns: collector.columns,
            output: root_output(statement),
            written: written_relation(statement),
            scope_candidates: collector.scope_candidates,
        }
    }

    /// Restore a relation when the legacy normalized reference identifies one
    /// unique AST spelling. If spellings collide, this returns the legacy
    /// components without claiming quote styles; callers can inspect
    /// [`Self::relation_candidates`] to handle that ambiguity explicitly.
    pub(super) fn relation(&self, old: &TableRef) -> Relation {
        let candidates = self.relation_candidates(old);
        if let [relation] = candidates.as_slice() {
            return relation.clone();
        }
        relation_from_old(old)
    }

    pub(super) fn written_relation(&self, old: &TableRef) -> Relation {
        self.written
            .as_ref()
            .filter(|relation| relation_matches(relation, old))
            .cloned()
            .unwrap_or_else(|| self.relation(old))
    }

    pub(super) fn has_column_reference(&self, name: &str) -> bool {
        self.columns
            .iter()
            .any(|column| legacy_ident_matches(&column.ident, name))
    }

    /// Return a column spelling when matching occurrences agree on its quote
    /// context. Relation and alias identifiers are intentionally omitted.
    /// With no matching occurrence, retain the normalized unquoted spelling;
    /// conflicting occurrence contexts return `None`.
    pub(super) fn lookup_ident(&self, name: &str) -> Option<Ident> {
        let mut candidates = self
            .columns
            .iter()
            .filter(|column| legacy_ident_matches(&column.ident, name))
            .map(|column| column.ident.clone())
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        match candidates.as_slice() {
            [candidate] => Some(candidate.clone()),
            [] => Some(ident(name)),
            _ => None,
        }
    }

    /// Restore a projected reference's SQL spelling by output position.
    pub(super) fn output_reference_ident(&self, index: usize, column: &str) -> Ident {
        let mut candidates = self
            .output
            .get(index)
            .into_iter()
            .flat_map(|slot| slot.source_columns.iter())
            .filter(|source| legacy_ident_matches(&source.ident, column))
            .map(|source| source.ident.clone())
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        match candidates.as_slice() {
            [ident] => ident.clone(),
            _ => ident(column),
        }
    }

    pub(super) fn is_star_expansion(&self, index: usize, total: usize) -> bool {
        let mut stars = self
            .output
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.is_star);
        let Some((first_index, _)) = stars.next() else {
            return false;
        };
        if stars.next().is_none() {
            let width = total.saturating_sub(self.output.len() - 1);
            return index >= first_index && index < first_index + width;
        }
        self.output.iter().all(|slot| slot.is_star)
    }

    /// All AST relations that could have produced a legacy normalized ref.
    pub(super) fn relation_candidates(&self, old: &TableRef) -> Vec<Relation> {
        let mut candidates = self
            .relations
            .iter()
            .filter(|relation| relation_matches(relation, old))
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        candidates
    }

    pub(super) fn candidate(&self, old: &TableRef) -> Candidate {
        if old.catalog.is_none() && old.schema.is_none() && self.relation_candidates(old).is_empty()
        {
            let mut candidates: Vec<_> = self
                .scope_candidates
                .iter()
                .filter(|candidate| match candidate {
                    Candidate::Cte(name) | Candidate::Derived(Some(name)) => {
                        legacy_ident_matches(name, &old.table)
                    }
                    _ => false,
                })
                .cloned()
                .collect();
            candidates.sort();
            candidates.dedup();
            if let [candidate] = candidates.as_slice() {
                return candidate.clone();
            }
        }
        Candidate::Relation(self.relation(old))
    }

    /// Classify a legacy candidate using providers visible to the projected
    /// column occurrence, including occurrences inherited through inputs.
    pub(super) fn candidates_for_output(
        &self,
        index: usize,
        column: &str,
        old: &TableRef,
    ) -> Vec<Candidate> {
        let occurrences = self.output_source_columns(index, column);
        let unqualified = old.catalog.is_none() && old.schema.is_none();
        let mut candidates = occurrences
            .into_iter()
            .flat_map(|occurrence| occurrence.scope_candidates.iter())
            .filter(|candidate| match candidate {
                Candidate::Relation(relation) => relation_matches(relation, old),
                Candidate::Cte(name) | Candidate::Derived(Some(name)) => {
                    unqualified && name.value == old.table
                }
                Candidate::Derived(None) => false,
            })
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        if candidates.is_empty() {
            candidates.push(self.candidate(old));
        }
        candidates
    }

    /// Restore a uniquely matching identifier spelling from the statement.
    /// If matching AST occurrences disagree, return the supplied spelling
    /// without asserting a quote style.
    pub(super) fn ident(&self, old: &str) -> Ident {
        let mut candidates = self
            .relations
            .iter()
            .flat_map(|relation| {
                relation
                    .catalog
                    .iter()
                    .chain(relation.schema.iter())
                    .chain(std::iter::once(&relation.name))
            })
            .chain(self.columns.iter().map(|column| &column.ident))
            .chain(self.output.iter().filter_map(|slot| match &slot.name {
                OutputName::Explicit(ident) | OutputName::Inferred(ident) => Some(ident),
                OutputName::Anonymous => None,
            }))
            .filter(|candidate| legacy_ident_matches(candidate, old))
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        if let [candidate] = candidates.as_slice() {
            candidate.clone()
        } else {
            ident(old)
        }
    }

    /// Restore a column's spelling using relation-qualified AST context when
    /// available. A unique matching spelling wins; otherwise the legacy
    /// spelling is returned as unquoted by [`ident`].
    pub(super) fn source_ident(&self, old_relation: &TableRef, old_column: &str) -> Ident {
        let relations = self.relation_candidates(old_relation);
        let relation_names = relations
            .iter()
            .map(|relation| relation.name.value.as_str())
            .chain(
                self.relation_aliases
                    .iter()
                    .filter_map(|(alias, relation)| {
                        relation_matches(relation, old_relation).then_some(alias.as_str())
                    }),
            )
            .collect::<Vec<_>>();
        let mut candidates = self
            .columns
            .iter()
            .filter(|column| legacy_ident_matches(&column.ident, old_column))
            .filter(|column| {
                column.qualifier.as_ref().is_none_or(|qualifier| {
                    relation_names
                        .iter()
                        .any(|name| legacy_component_matches(qualifier, name))
                        || self.relation_aliases.iter().any(|(alias, relation)| {
                            relation_matches(relation, old_relation)
                                && legacy_component_matches(qualifier, alias)
                        })
                })
            })
            .map(|column| column.ident.clone())
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        if let [ident] = candidates.as_slice() {
            ident.clone()
        } else {
            ident(old_column)
        }
    }

    /// Resolve the source relation from a root projection's qualified column
    /// reference. This preserves AST context when legacy table names collide.
    pub(super) fn relation_for_output(
        &self,
        mapping_index: usize,
        old_table: &TableRef,
        old_column: &str,
    ) -> Option<Relation> {
        let mut candidates = self
            .output_source_columns(mapping_index, old_column)
            .into_iter()
            .filter_map(|column| {
                let qualifier = column.qualifier.as_deref()?;
                let mut matches = self
                    .relation_candidates(old_table)
                    .into_iter()
                    .filter(|relation| legacy_component_matches(qualifier, &relation.name.value))
                    .chain(
                        self.relation_aliases
                            .iter()
                            .filter(|(alias, relation)| {
                                relation_matches(relation, old_table)
                                    && legacy_component_matches(qualifier, alias)
                            })
                            .map(|(_, relation)| relation.clone()),
                    )
                    .collect::<Vec<_>>();
                matches.sort();
                matches.dedup();
                match matches.as_slice() {
                    [relation] => Some(relation.clone()),
                    _ => None,
                }
            })
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        if let [relation] = candidates.as_slice() {
            Some(relation.clone())
        } else {
            None
        }
    }

    /// Restore a source identifier using the selected root expression before
    /// consulting statement-wide occurrences.
    pub(super) fn source_ident_for_output(
        &self,
        mapping_index: usize,
        old_table: &TableRef,
        old_column: &str,
    ) -> Ident {
        let mut candidates = self
            .output_source_columns(mapping_index, old_column)
            .into_iter()
            .filter(|column| {
                column
                    .owner
                    .as_ref()
                    .is_none_or(|owner| relation_matches(owner, old_table))
            })
            .filter(|column| {
                let Some(qualifier) = column.qualifier.as_deref() else {
                    return true;
                };
                self.relation_for_output(mapping_index, old_table, old_column)
                    .is_some_and(|relation| {
                        legacy_component_matches(qualifier, &relation.name.value)
                            || self.relation_aliases.iter().any(|(alias, target)| {
                                target == &relation && legacy_component_matches(qualifier, alias)
                            })
                    })
            })
            .map(|column| column.ident.clone())
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();
        if let [ident] = candidates.as_slice() {
            ident.clone()
        } else {
            self.source_ident(old_table, old_column)
        }
    }

    fn output_source_columns(
        &self,
        mapping_index: usize,
        old_column: &str,
    ) -> Vec<&ColumnOccurrence> {
        if !self.output.iter().any(|slot| slot.is_star)
            && let Some(slot) = self.output.get(mapping_index)
        {
            let exact = slot
                .source_columns
                .iter()
                .filter(|column| legacy_ident_matches(&column.ident, old_column))
                .collect::<Vec<_>>();
            if !exact.is_empty() {
                return exact;
            }
        }
        self.output
            .iter()
            .flat_map(|slot| slot.source_columns.iter())
            .filter(|column| legacy_ident_matches(&column.ident, old_column))
            .collect()
    }

    /// Recover a legacy output name by its position after catalog star
    /// expansion. The total mapping count determines the width of a sole star,
    /// preserving duplicate aliases and their quote styles after that star.
    pub(super) fn output_name_at(
        &self,
        mapping_index: usize,
        total_mapping_count: usize,
        old_name: &str,
    ) -> OutputName {
        let star_positions = self
            .output
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| slot.is_star.then_some(index))
            .collect::<Vec<_>>();
        if star_positions.len() == 1 {
            let star_index = star_positions[0];
            let explicit_count = self.output.len() - 1;
            if total_mapping_count >= explicit_count {
                let expanded_width = total_mapping_count - explicit_count;
                let mut mapped_index = 0;
                for (slot_index, slot) in self.output.iter().enumerate() {
                    if slot_index == star_index {
                        if mapping_index >= mapped_index
                            && mapping_index < mapped_index + expanded_width
                        {
                            return self.fallback_output_name(old_name);
                        }
                        mapped_index += expanded_width;
                    } else {
                        if mapping_index == mapped_index {
                            return if output_name_matches(&slot.name, old_name) {
                                slot.name.clone()
                            } else {
                                self.fallback_output_name(old_name)
                            };
                        }
                        mapped_index += 1;
                    }
                }
            }
        } else if star_positions.is_empty() {
            return self
                .output
                .get(mapping_index)
                .filter(|slot| output_name_matches(&slot.name, old_name))
                .map_or_else(
                    || self.fallback_output_name(old_name),
                    |slot| slot.name.clone(),
                );
        } else if let (Some(first_star), Some(last_star)) =
            (star_positions.first(), star_positions.last())
        {
            let prefix = &self.output[..*first_star];
            if mapping_index < prefix.len() {
                let slot = &prefix[mapping_index];
                return if output_name_matches(&slot.name, old_name) {
                    slot.name.clone()
                } else {
                    self.fallback_output_name(old_name)
                };
            }
            let suffix = &self.output[last_star + 1..];
            let suffix_start = total_mapping_count.saturating_sub(suffix.len());
            if mapping_index >= suffix_start {
                let slot_index = last_star + 1 + (mapping_index - suffix_start);
                if let Some(slot) = self.output.get(slot_index) {
                    return if output_name_matches(&slot.name, old_name) {
                        slot.name.clone()
                    } else {
                        self.fallback_output_name(old_name)
                    };
                }
            }
            let mut candidates = self.output[*first_star + 1..*last_star]
                .iter()
                .filter(|slot| !slot.is_star && output_name_matches(&slot.name, old_name));
            if let Some(slot) = candidates.next()
                && candidates.next().is_none()
            {
                return slot.name.clone();
            }
        }

        let has_any_star = self.output.iter().any(|slot| slot.is_star);
        if !has_any_star
            && let Some(slot) = self.output.get(mapping_index)
            && !slot.is_star
            && output_name_matches(&slot.name, old_name)
        {
            return slot.name.clone();
        }
        let mut matches = self
            .output
            .iter()
            .enumerate()
            .filter(|(_, slot)| !slot.is_star && output_name_matches(&slot.name, old_name))
            .collect::<Vec<_>>();
        if matches.len() == 1 {
            return matches[0].1.name.clone();
        }

        if matches.is_empty() {
            return self.fallback_output_name(old_name);
        }
        // Equal legacy names can denote distinct quoted names. Preserve a name
        // only when all matching AST slots agree exactly.
        let first = matches.remove(0).1.name.clone();
        if matches.iter().all(|(_, slot)| slot.name == first) {
            first
        } else {
            self.fallback_output_name(old_name)
        }
    }

    fn fallback_output_name(&self, old_name: &str) -> OutputName {
        if old_name == "?column?" || old_name == "*" {
            OutputName::Anonymous
        } else {
            OutputName::Explicit(self.ident(old_name))
        }
    }

    /// Find a row-value alias from the corresponding root projection item.
    pub(super) fn row_value_alias(&self, output_index: usize, old_name: &str) -> Option<Ident> {
        let matching = self
            .output
            .iter()
            .filter(|slot| !slot.is_star && output_name_matches(&slot.name, old_name))
            .collect::<Vec<_>>();
        if matching.len() == 1 {
            return matching[0].row_value.clone();
        }
        self.output
            .get(output_index)
            .filter(|slot| !slot.is_star && output_name_matches(&slot.name, old_name))
            .and_then(|slot| slot.row_value.clone())
    }

    /// Whether the legacy reference collapses distinct source quote contexts.
    pub(super) fn relation_is_ambiguous(&self, old: &TableRef) -> bool {
        self.relation_candidates(old).len() > 1
    }
}

/// Construct an unquoted identifier for values that have no usable AST match.
pub(super) fn ident(value: &str) -> Ident {
    Ident::unquoted(value)
}

#[derive(Default)]
struct AstMetadataCollector {
    relations: Vec<Relation>,
    relation_aliases: Vec<(String, Relation)>,
    columns: Vec<ColumnOccurrence>,
    scope_candidates: Vec<Candidate>,
    cte_scopes: Vec<CteScopeFrame>,
}

#[derive(Default)]
struct CteScopeFrame {
    inherited: Vec<sqlparser::ast::Ident>,
    local: Vec<sqlparser::ast::Ident>,
    active: Vec<sqlparser::ast::Ident>,
    definitions: Vec<(*const Query, usize)>,
    recursive: bool,
}

impl Visitor for AstMetadataCollector {
    type Break = ();

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<Self::Break> {
        if let TableFactor::Table { name, alias, .. } = factor
            && let Some(relation) = relation_from_object_name(name)
        {
            let cte = if name.0.len() == 1 {
                self.cte_scopes.last().and_then(|scope| {
                    scope
                        .active
                        .iter()
                        .rev()
                        .find(|cte| sql_ident_matches(cte, &relation.name))
                })
            } else {
                None
            };
            if let Some(cte) = cte {
                self.scope_candidates
                    .push(Candidate::Cte(from_sql_ident(cte)));
            } else {
                self.relations.push(relation.clone());
                if let Some(alias) = alias {
                    self.relation_aliases
                        .push((alias.name.value.clone(), relation));
                }
            }
        }
        if let TableFactor::Derived {
            alias: Some(alias), ..
        } = factor
        {
            self.scope_candidates
                .push(Candidate::Derived(Some(from_sql_ident(&alias.name))));
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        let inherited = self
            .cte_scopes
            .last()
            .map(|parent| {
                let definition_index = parent.definitions.iter().position(|(definition, _)| {
                    std::ptr::eq(*definition, std::ptr::from_ref(query))
                });
                let Some(index) = definition_index else {
                    return parent.active.clone();
                };
                let mut visible = parent.inherited.clone();
                visible.extend(parent.local[..index].iter().cloned());
                if parent.recursive {
                    visible.push(parent.local[index].clone());
                }
                visible
            })
            .unwrap_or_default();
        let with = query.with.as_ref();
        let local = with
            .map(|with| {
                with.cte_tables
                    .iter()
                    .map(|cte| cte.alias.name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut active = inherited.clone();
        active.extend(local.iter().cloned());
        self.cte_scopes.push(CteScopeFrame {
            inherited,
            local,
            active,
            definitions: with
                .map(|with| {
                    with.cte_tables
                        .iter()
                        .enumerate()
                        .map(|(index, cte)| (std::ptr::from_ref(cte.query.as_ref()), index))
                        .collect()
                })
                .unwrap_or_default(),
            recursive: with.is_some_and(|with| with.recursive),
        });
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &Query) -> ControlFlow<Self::Break> {
        self.cte_scopes.pop();
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
        match expr {
            Expr::Identifier(column) => self.columns.push(ColumnOccurrence {
                ident: from_sql_ident(column),
                qualifier: None,
                owner: None,
                scope_candidates: Vec::new(),
            }),
            Expr::CompoundIdentifier(parts) => {
                if let Some(column) = parts.last() {
                    self.columns.push(ColumnOccurrence {
                        ident: from_sql_ident(column),
                        qualifier: parts
                            .get(parts.len().saturating_sub(2))
                            .map(|p| p.value.clone()),
                        owner: None,
                        scope_candidates: Vec::new(),
                    });
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

fn root_output(statement: &Statement) -> Vec<OutputSlot> {
    match statement {
        Statement::Query(query) => query_output(query),
        Statement::CreateTable(create) => create
            .query
            .as_deref()
            .map(query_output)
            .unwrap_or_default(),
        Statement::Insert(insert) => insert
            .source
            .as_deref()
            .map(query_output)
            .unwrap_or_default(),
        Statement::Update(update) => update
            .assignments
            .iter()
            .flat_map(|assignment| assignment_target_idents(&assignment.target))
            .map(|name| OutputSlot {
                name: OutputName::Explicit(name),
                is_star: false,
                row_value: None,
                source_columns: Vec::new(),
            })
            .collect(),
        Statement::Merge(merge) => merge
            .clauses
            .iter()
            .flat_map(|clause| match &clause.action {
                ast::MergeAction::Update(update) => match &update.kind {
                    ast::MergeUpdateKind::Set(assignments) => assignments
                        .iter()
                        .flat_map(|assignment| assignment_target_idents(&assignment.target))
                        .map(|name| OutputSlot {
                            name: OutputName::Explicit(name),
                            is_star: false,
                            row_value: None,
                            source_columns: Vec::new(),
                        })
                        .collect(),
                    ast::MergeUpdateKind::Wildcard => vec![star_slot()],
                },
                ast::MergeAction::Insert(insert) => match &insert.kind {
                    ast::MergeInsertKind::Values(_) => insert
                        .columns
                        .iter()
                        .filter_map(|name| name.0.last().and_then(|part| part.as_ident()))
                        .map(|name| OutputSlot {
                            name: OutputName::Explicit(from_sql_ident(name)),
                            is_star: false,
                            row_value: None,
                            source_columns: Vec::new(),
                        })
                        .collect(),
                    ast::MergeInsertKind::Wildcard | ast::MergeInsertKind::Row => vec![star_slot()],
                },
                ast::MergeAction::Delete { .. } | ast::MergeAction::DoNothing { .. } => vec![],
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn assignment_target_idents(target: &ast::AssignmentTarget) -> Vec<Ident> {
    match target {
        ast::AssignmentTarget::ColumnName(name) => name
            .0
            .last()
            .and_then(|part| part.as_ident())
            .map(from_sql_ident)
            .into_iter()
            .collect(),
        ast::AssignmentTarget::Tuple(names) => names
            .iter()
            .filter_map(|name| name.0.last().and_then(|part| part.as_ident()))
            .map(from_sql_ident)
            .collect(),
    }
}

fn star_slot() -> OutputSlot {
    OutputSlot {
        name: OutputName::Anonymous,
        is_star: true,
        row_value: None,
        source_columns: Vec::new(),
    }
}

fn relation_from_object_name(name: &ObjectName) -> Option<Relation> {
    let parts = name
        .0
        .iter()
        .map(|part| part.as_ident())
        .collect::<Option<Vec<_>>>()?;
    match parts.as_slice() {
        [table] => Some(Relation {
            catalog: None,
            schema: None,
            name: from_sql_ident(table),
        }),
        [schema, table] => Some(Relation {
            catalog: None,
            schema: Some(from_sql_ident(schema)),
            name: from_sql_ident(table),
        }),
        [catalog, schema, table] => Some(Relation {
            catalog: Some(from_sql_ident(catalog)),
            schema: Some(from_sql_ident(schema)),
            name: from_sql_ident(table),
        }),
        _ => None,
    }
}

fn relation_from_old(old: &TableRef) -> Relation {
    Relation {
        catalog: old.catalog.as_deref().map(ident),
        schema: old.schema.as_deref().map(ident),
        name: ident(&old.table),
    }
}

fn written_relation(statement: &Statement) -> Option<Relation> {
    fn table(factor: &TableFactor) -> Option<Relation> {
        if let TableFactor::Table { name, .. } = factor {
            relation_from_object_name(name)
        } else {
            None
        }
    }
    match statement {
        Statement::Insert(insert) => match &insert.table {
            ast::TableObject::TableName(name) => relation_from_object_name(name),
            _ => None,
        },
        Statement::CreateTable(create) => relation_from_object_name(&create.name),
        Statement::Update(update) => table(&update.table.relation),
        Statement::Delete(delete) => {
            let (ast::FromTable::WithFromKeyword(from) | ast::FromTable::WithoutKeyword(from)) =
                &delete.from;
            from.first().and_then(|item| table(&item.relation))
        }
        Statement::Merge(merge) => table(&merge.table),
        _ => None,
    }
}

fn relation_matches(relation: &Relation, old: &TableRef) -> bool {
    component_matches(relation.catalog.as_ref(), old.catalog.as_deref())
        && component_matches(relation.schema.as_ref(), old.schema.as_deref())
        && legacy_ident_matches(&relation.name, &old.table)
}

fn component_matches(candidate: Option<&Ident>, old: Option<&str>) -> bool {
    match (candidate, old) {
        (None, None) => true,
        (Some(candidate), Some(old)) => legacy_ident_matches(candidate, old),
        _ => false,
    }
}

fn legacy_ident_matches(candidate: &Ident, old: &str) -> bool {
    candidate.value == old || (candidate.quote.is_none() && candidate.value.to_lowercase() == old)
}

fn legacy_component_matches(candidate: &str, name: &str) -> bool {
    candidate == name || candidate.to_lowercase() == name.to_lowercase()
}

fn sql_ident_matches(candidate: &sqlparser::ast::Ident, reference: &Ident) -> bool {
    let candidate_ident = from_sql_ident(candidate);
    if candidate_ident.quote.is_some() || reference.quote.is_some() {
        candidate_ident == *reference
    } else {
        candidate_ident.value.eq_ignore_ascii_case(&reference.value)
    }
}

fn output_name_matches(name: &OutputName, old: &str) -> bool {
    match name {
        OutputName::Explicit(ident) | OutputName::Inferred(ident) => {
            legacy_ident_matches(ident, old)
        }
        OutputName::Anonymous => old == "?column?",
    }
}

fn from_sql_ident(ident: &sqlparser::ast::Ident) -> Ident {
    let quote = match ident.quote_style {
        Some('"') => Some(Quote::Double),
        Some('`') => Some(Quote::Backtick),
        Some('[') => Some(Quote::Bracket),
        Some('\'') => Some(Quote::Single),
        _ => None,
    };
    Ident {
        value: ident.value.clone(),
        quote,
    }
}
