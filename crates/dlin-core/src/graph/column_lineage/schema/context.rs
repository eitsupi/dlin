//! What the analysis knows about the columns of a dependency, and the catalog
//! built from it.
//!
//! A model provides its SQL output downstream. The columns declared in YAML
//! are kept as a separate record and only stand in when there is no usable SQL
//! output. The two are never merged into a union.

use std::collections::HashMap;
use std::sync::Arc;

use super::evidence::{EvidenceId, SchemaEvidence, yaml_schema_evidence};
use super::sql_output::SqlOutputSchema;
use crate::graph::column_lineage::backend::sqllineage::{analyze_sql, single_lineage_statement};
use crate::graph::column_lineage::backend::{DlinDialect, catalog::CatalogSnapshot};
use crate::graph::column_lineage::relation::RelationRef;
use crate::parser::manifest::{Manifest, ManifestNode};

/// The outcome of inferring a node's output columns from its SQL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SqlOutputState {
    /// The SQL was analysed as a single lineage-bearing statement.
    Available(SchemaEvidence),
    /// There is SQL, but it could not be analysed.
    Failed(String),
    /// The node has no SQL to analyse.
    Unavailable,
}

/// Why YAML columns were adopted although the node has a SQL definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FallbackReason {
    SqlAnalysisFailed { detail: String },
    SqlUnavailable,
}

/// Which record a dependency provides downstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AdoptedSchema {
    /// The SQL output of the node.
    Sql(EvidenceId),
    /// The YAML declaration of a node that has no SQL definition by nature,
    /// such as a source.
    Yaml(EvidenceId),
    /// The YAML declaration of a model whose SQL gave no schema.
    YamlFallback {
        evidence: EvidenceId,
        reason: FallbackReason,
    },
    /// Nothing is known about the columns.
    None,
}

/// The YAML and SQL records for one dependency, and the one it provides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NodeSchemaKnowledge {
    pub(crate) sql_output: SqlOutputState,
    pub(crate) yaml_declaration: Option<SchemaEvidence>,
    pub(crate) adopted: AdoptedSchema,
}

impl NodeSchemaKnowledge {
    /// A source has no SQL; its YAML declaration is its schema.
    fn for_source(yaml_declaration: Option<SchemaEvidence>) -> Self {
        let adopted = yaml_declaration
            .as_ref()
            .map_or(AdoptedSchema::None, |yaml| {
                AdoptedSchema::Yaml(yaml.id.clone())
            });
        Self {
            sql_output: SqlOutputState::Unavailable,
            yaml_declaration,
            adopted,
        }
    }

    /// The SQL output wins whenever there is one, even if it is open. YAML
    /// only fills in when the SQL gave no schema.
    fn for_model(sql_output: SqlOutputState, yaml_declaration: Option<SchemaEvidence>) -> Self {
        let fallback = |reason: FallbackReason| {
            yaml_declaration
                .as_ref()
                .map_or(AdoptedSchema::None, |yaml| AdoptedSchema::YamlFallback {
                    evidence: yaml.id.clone(),
                    reason,
                })
        };
        let adopted = match &sql_output {
            SqlOutputState::Available(sql) => AdoptedSchema::Sql(sql.id.clone()),
            SqlOutputState::Failed(detail) => fallback(FallbackReason::SqlAnalysisFailed {
                detail: detail.clone(),
            }),
            SqlOutputState::Unavailable => fallback(FallbackReason::SqlUnavailable),
        };
        Self {
            sql_output,
            yaml_declaration,
            adopted,
        }
    }

    /// The evidence this dependency provides to the models that read it.
    pub(crate) fn adopted_evidence(&self) -> Option<&SchemaEvidence> {
        let id = match &self.adopted {
            AdoptedSchema::Sql(id)
            | AdoptedSchema::Yaml(id)
            | AdoptedSchema::YamlFallback { evidence: id, .. } => id,
            AdoptedSchema::None => return None,
        };
        let sql = match &self.sql_output {
            SqlOutputState::Available(sql) => Some(sql),
            SqlOutputState::Failed(_) | SqlOutputState::Unavailable => None,
        };
        sql.into_iter()
            .chain(self.yaml_declaration.as_ref())
            .find(|evidence| &evidence.id == id)
    }

    /// Column names only the YAML declares and names only the SQL output
    /// lists, compared by the dialect's column key.
    #[cfg(test)]
    pub(crate) fn name_difference(&self, dialect: sqllineage::Dialect) -> NameDifference {
        let keys = |evidence: Option<&SchemaEvidence>| -> Vec<(String, String)> {
            evidence
                .into_iter()
                .flat_map(|evidence| &evidence.columns)
                .map(|column| {
                    (
                        column.name.column_key(dialect).into_owned(),
                        column.name.value.clone(),
                    )
                })
                .collect()
        };
        let sql = match &self.sql_output {
            SqlOutputState::Available(sql) => Some(sql),
            SqlOutputState::Failed(_) | SqlOutputState::Unavailable => None,
        };
        let sql = keys(sql);
        let yaml = keys(self.yaml_declaration.as_ref());
        let only = |from: &[(String, String)], other: &[(String, String)]| {
            from.iter()
                .filter(|(key, _)| !other.iter().any(|(other_key, _)| other_key == key))
                .map(|(_, name)| name.clone())
                .collect()
        };
        NameDifference {
            yaml_only: only(&yaml, &sql),
            sql_only: only(&sql, &yaml),
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NameDifference {
    pub(crate) yaml_only: Vec<String>,
    pub(crate) sql_only: Vec<String>,
}

/// Builds the catalog of the dependencies of a node for one manifest and
/// dialect.
///
/// Knowledge is memoized by dependency unique id, including failures and
/// unavailable SQL. Each model is analysed with the catalog of its own
/// dependencies, so the order of a `SELECT *` chain is carried from model to
/// model.
///
/// A malformed manifest can contain a dependency cycle. A back-edge to a node
/// that is being visited gets no entry, and a result that was computed with
/// such an edge cut is not memoized, so no result depends on where the
/// traversal started.
pub(in crate::graph::column_lineage) struct SchemaContext<'a> {
    manifest: &'a Manifest,
    dialect: DlinDialect,
    memo: HashMap<String, Arc<NodeSchemaKnowledge>>,
    visiting: Vec<String>,
}

impl<'a> SchemaContext<'a> {
    pub(in crate::graph::column_lineage) fn new(
        manifest: &'a Manifest,
        dialect: DlinDialect,
    ) -> Self {
        Self {
            manifest,
            dialect,
            memo: HashMap::new(),
            visiting: Vec::new(),
        }
    }

    /// The catalog of the relations `node` reads, or `None` when no dependency
    /// has a known schema.
    pub(in crate::graph::column_lineage) fn catalog_for(
        &mut self,
        node: &ManifestNode,
    ) -> Option<CatalogSnapshot> {
        self.visiting.push(node.unique_id.clone());
        let catalog = self.dependency_catalog(node, &mut false);
        self.visiting.pop();
        catalog
    }

    /// The knowledge about one dependency, or `None` for an unknown id or a
    /// back-edge of a dependency cycle.
    #[cfg(test)]
    pub(in crate::graph::column_lineage) fn knowledge_of(
        &mut self,
        id: &str,
    ) -> Option<Arc<NodeSchemaKnowledge>> {
        self.dependency_knowledge(id, &mut false)
    }

    fn dependency_catalog(
        &mut self,
        node: &ManifestNode,
        cycle_affected: &mut bool,
    ) -> Option<CatalogSnapshot> {
        let manifest = self.manifest;
        let mut catalog = CatalogSnapshot::new();
        let mut has_entries = false;

        for dep_id in &node.depends_on.nodes {
            let Some(knowledge) = self.dependency_knowledge(dep_id, cycle_affected) else {
                continue;
            };
            let Some(evidence) = knowledge.adopted_evidence() else {
                continue;
            };
            if let Some(dep_node) = manifest.nodes.get(dep_id) {
                has_entries |= register_model_relation(
                    &mut catalog,
                    dep_node.database.as_deref(),
                    dep_node.schema.as_deref(),
                    dep_node.relation_name(),
                    evidence.clone(),
                );
            } else if let Some(source) = manifest.sources.get(dep_id) {
                let physical_identifier = source.identifier.as_deref().unwrap_or(&source.name);
                has_entries |= register_source_relation(
                    &mut catalog,
                    source.database.as_deref(),
                    source.schema.as_deref(),
                    physical_identifier,
                    &source.source_name,
                    &source.name,
                    evidence.clone(),
                );
            }
        }

        has_entries.then_some(catalog)
    }

    fn dependency_knowledge(
        &mut self,
        id: &str,
        cycle_affected: &mut bool,
    ) -> Option<Arc<NodeSchemaKnowledge>> {
        if let Some(knowledge) = self.memo.get(id) {
            return Some(Arc::clone(knowledge));
        }
        if self.visiting.iter().any(|visiting| visiting == id) {
            *cycle_affected = true;
            return None;
        }
        let manifest = self.manifest;
        if let Some(node) = manifest.nodes.get(id) {
            self.visiting.push(id.to_owned());
            let mut affected = false;
            let knowledge = Arc::new(self.model_knowledge(id, node, &mut affected));
            self.visiting.pop();
            if affected {
                *cycle_affected = true;
            } else {
                self.memo.insert(id.to_owned(), Arc::clone(&knowledge));
            }
            Some(knowledge)
        } else if let Some(source) = manifest.sources.get(id) {
            let yaml = yaml_schema_evidence(id, &source.columns, None, self.dialect);
            let knowledge = Arc::new(NodeSchemaKnowledge::for_source(yaml));
            self.memo.insert(id.to_owned(), Arc::clone(&knowledge));
            Some(knowledge)
        } else {
            None
        }
    }

    fn model_knowledge(
        &mut self,
        id: &str,
        node: &ManifestNode,
        cycle_affected: &mut bool,
    ) -> NodeSchemaKnowledge {
        let yaml = yaml_schema_evidence(
            id,
            &node.columns,
            node.config.contract.as_ref(),
            self.dialect,
        );
        let sql_output = match &node.compiled_code {
            None => SqlOutputState::Unavailable,
            Some(code) => {
                let catalog = self.dependency_catalog(node, cycle_affected);
                match sql_output_evidence(code, self.dialect, catalog.as_ref(), id) {
                    Ok(evidence) => SqlOutputState::Available(evidence),
                    Err(detail) => SqlOutputState::Failed(detail),
                }
            }
        };
        NodeSchemaKnowledge::for_model(sql_output, yaml)
    }
}

/// The output schema of `sql`, which must be a single lineage-bearing
/// statement that analyses without an error.
fn sql_output_evidence(
    sql: &str,
    dialect: DlinDialect,
    catalog: Option<&CatalogSnapshot>,
    subject: &str,
) -> Result<SchemaEvidence, String> {
    let analysis = analyze_sql(sql, dialect, catalog)?;
    let statement = single_lineage_statement(&analysis)?;
    Ok(SqlOutputSchema::from_output(&statement.output).to_evidence(subject))
}

fn register_model_relation(
    schema: &mut CatalogSnapshot,
    database: Option<&str>,
    schema_name: Option<&str>,
    relation_name: &str,
    evidence: SchemaEvidence,
) -> bool {
    let relation = RelationRef::from_manifest(database, schema_name, relation_name);
    let was_present = schema.contains_relation(&relation);
    schema.add_relation(relation.clone(), relation.render(), evidence);
    if relation.qualification_len() > 1 {
        schema.add_alias(
            RelationRef::from_manifest(None, None, relation_name),
            relation,
        );
    }
    !was_present
}

fn register_source_relation(
    schema: &mut CatalogSnapshot,
    database: Option<&str>,
    schema_name: Option<&str>,
    physical_identifier: &str,
    source_name: &str,
    logical_name: &str,
    evidence: SchemaEvidence,
) -> bool {
    let physical = RelationRef::from_manifest(database, schema_name, physical_identifier);
    let was_present = schema.contains_relation(&physical);
    schema.add_relation(physical.clone(), physical.render(), evidence);

    let aliases = [
        RelationRef::from_manifest(None, schema_name, physical_identifier),
        RelationRef::from_manifest(None, None, physical_identifier),
        RelationRef::from_manifest(None, None, logical_name),
        RelationRef::from_manifest(None, Some(source_name), logical_name),
    ];
    for alias in aliases {
        if alias != physical {
            schema.add_alias(alias, physical.clone());
        }
    }
    !was_present
}
