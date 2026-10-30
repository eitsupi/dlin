//! Output categories and lexical candidate kinds retained until direct result generation.

use crate::keys::{candidate_key, sort_dedup_by_key};
use crate::types::TableRef;
use crate::{Candidate, Dialect, Ident, OutputName, Relation};
use sqlparser::ast::{self, ObjectName, Query, Statement, TableFactor, Visit, Visitor};
use std::ops::ControlFlow;
mod projection;
use projection::query_output;

/// Output-shape and lexical provider hints for the conversion boundary.
pub(super) struct StatementMetadata {
    output: Vec<OutputSlot>,
    dialect: Dialect,
    relations: Vec<Relation>,
    scope_candidates: Vec<Candidate>,
}

#[derive(Clone)]
struct ColumnOccurrence {
    ident: Ident,
    qualifier: Option<Ident>,
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
    pub(super) fn dialect(&self) -> Dialect {
        self.dialect
    }

    pub(super) fn new(statement: &Statement, dialect: Dialect) -> Self {
        let mut providers = ProviderCollector {
            dialect,
            ..Default::default()
        };
        let _ = statement.visit(&mut providers);
        Self {
            output: root_output(statement, dialect),
            dialect,
            relations: providers.relations,
            scope_candidates: providers.scope_candidates,
        }
    }

    pub(super) fn relation(table: &TableRef) -> Relation {
        Relation {
            catalog: table.catalog.clone(),
            schema: table.schema.clone(),
            name: table.table.clone(),
        }
    }

    pub(super) fn candidates_for_output(
        &self,
        index: usize,
        column: &Ident,
        table: &TableRef,
    ) -> Vec<Candidate> {
        let matching = |occurrence: &&ColumnOccurrence| {
            occurrence.ident.column_key(self.dialect) == column.column_key(self.dialect)
        };
        let mut occurrences = self
            .output
            .get(index)
            .into_iter()
            .flat_map(|slot| slot.source_columns.iter())
            .filter(matching)
            .collect::<Vec<_>>();
        if self.output.iter().any(|slot| slot.is_star) || occurrences.is_empty() {
            occurrences = self
                .output
                .iter()
                .flat_map(|slot| slot.source_columns.iter())
                .filter(matching)
                .collect();
        }
        let relation = Self::relation(table);
        let mut candidates = occurrences
            .into_iter()
            .flat_map(|occurrence| occurrence.scope_candidates.iter())
            .filter(|candidate| match candidate {
                Candidate::Relation(candidate) => relation_matches(candidate, table, self.dialect),
                Candidate::Cte(name) | Candidate::Derived(Some(name)) => {
                    table.catalog.is_none()
                        && table.schema.is_none()
                        && name.relation_key(self.dialect) == table.table.relation_key(self.dialect)
                }
                Candidate::Derived(None) => false,
            })
            .cloned()
            .collect::<Vec<_>>();
        sort_dedup_by_key(&mut candidates, |candidate| {
            candidate_key(candidate, self.dialect)
        });
        if candidates.is_empty() {
            let physical = self
                .relations
                .iter()
                .any(|relation| relation_matches(relation, table, self.dialect));
            let mut providers = self
                .scope_candidates
                .iter()
                .filter(|candidate| match candidate {
                    Candidate::Cte(name) | Candidate::Derived(Some(name)) => {
                        table.catalog.is_none()
                            && table.schema.is_none()
                            && name.relation_key(self.dialect)
                                == table.table.relation_key(self.dialect)
                    }
                    _ => false,
                })
                .cloned()
                .collect::<Vec<_>>();
            sort_dedup_by_key(&mut providers, |candidate| {
                candidate_key(candidate, self.dialect)
            });
            match (physical, providers.as_slice()) {
                (false, [provider]) => candidates.push(provider.clone()),
                _ => candidates.push(Candidate::Relation(relation)),
            }
        }
        candidates
    }

    /// Recover a legacy output name by its position after catalog star
    /// expansion. The total mapping count determines the width of a sole star,
    /// preserving duplicate aliases and their quote styles after that star.
    pub(super) fn output_name_at(
        &self,
        mapping_index: usize,
        total_mapping_count: usize,
        old_name: &Ident,
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
                            return Self::fallback_output_name(old_name);
                        }
                        mapped_index += expanded_width;
                    } else {
                        if mapping_index == mapped_index {
                            return if output_name_matches(&slot.name, old_name) {
                                slot.name.clone()
                            } else {
                                Self::fallback_output_name(old_name)
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
                    || Self::fallback_output_name(old_name),
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
                    Self::fallback_output_name(old_name)
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
                        Self::fallback_output_name(old_name)
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
            return Self::fallback_output_name(old_name);
        }
        // Equal legacy names can denote distinct quoted names. Preserve a name
        // only when all matching AST slots agree exactly.
        let first = matches.remove(0).1.name.clone();
        if matches.iter().all(|(_, slot)| slot.name == first) {
            first
        } else {
            Self::fallback_output_name(old_name)
        }
    }

    fn fallback_output_name(old_name: &Ident) -> OutputName {
        if old_name.quote.is_none() && matches!(old_name.value.as_str(), "?column?" | "*") {
            OutputName::Anonymous
        } else {
            OutputName::Explicit(old_name.clone())
        }
    }

    /// Find a row-value alias from the corresponding root projection item.
    pub(super) fn row_value_alias(&self, output_index: usize, old_name: &Ident) -> Option<Ident> {
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
}

#[derive(Default)]
struct ProviderCollector {
    dialect: Dialect,
    relations: Vec<Relation>,
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

impl Visitor for ProviderCollector {
    type Break = ();

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<Self::Break> {
        if let TableFactor::Table { name, .. } = factor
            && let Some(relation) = relation_from_object_name(name)
        {
            let cte = if name.0.len() == 1 {
                self.cte_scopes.last().and_then(|scope| {
                    scope
                        .active
                        .iter()
                        .rev()
                        .find(|cte| sql_ident_matches(cte, &relation.name, self.dialect))
                })
            } else {
                None
            };
            if let Some(cte) = cte {
                self.scope_candidates
                    .push(Candidate::Cte(from_sql_ident(cte)));
            } else {
                self.relations.push(relation);
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
}

fn root_output(statement: &Statement, dialect: Dialect) -> Vec<OutputSlot> {
    match statement {
        Statement::Query(query) => query_output(query, dialect),
        Statement::CreateTable(create) => create
            .query
            .as_deref()
            .map(|query| query_output(query, dialect))
            .unwrap_or_default(),
        Statement::Insert(insert) => insert
            .source
            .as_deref()
            .map(|query| query_output(query, dialect))
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

fn relation_matches(relation: &Relation, table: &TableRef, dialect: Dialect) -> bool {
    let table = StatementMetadata::relation(table);
    relation.catalog.as_ref().map(|id| id.relation_key(dialect))
        == table.catalog.as_ref().map(|id| id.relation_key(dialect))
        && relation.schema.as_ref().map(|id| id.relation_key(dialect))
            == table.schema.as_ref().map(|id| id.relation_key(dialect))
        && relation.name.relation_key(dialect) == table.name.relation_key(dialect)
}

fn sql_ident_matches(
    candidate: &sqlparser::ast::Ident,
    reference: &Ident,
    dialect: Dialect,
) -> bool {
    from_sql_ident(candidate).relation_key(dialect) == reference.relation_key(dialect)
}

// Selecting an AST output occurrence must not transfer spelling or quoting
// from a different identifier that merely has the same SQL lookup key.
fn output_name_matches(name: &OutputName, actual: &Ident) -> bool {
    match name {
        OutputName::Explicit(ident) | OutputName::Inferred(ident) => ident == actual,
        OutputName::Anonymous => actual.quote.is_none() && actual.value == "?column?",
    }
}

fn from_sql_ident(ident: &sqlparser::ast::Ident) -> Ident {
    Ident::from_ast(ident)
}
