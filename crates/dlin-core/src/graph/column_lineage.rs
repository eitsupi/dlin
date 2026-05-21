use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use polyglot_sql::{DialectType, Expression, Schema};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::parser::cache::hash_str;
use crate::parser::manifest::Manifest;

/// Column lineage result for a single model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelColumnLineage {
    pub model: String,
    /// Number of columns successfully traced
    pub traced_columns: usize,
    /// Total number of columns attempted (0 when model/SQL could not be loaded)
    pub total_columns: usize,
    pub columns: Vec<ColumnLineageEntry>,
    #[serde(default)]
    pub errors: Vec<String>,
}

/// Lineage for a single output column
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnLineageEntry {
    pub column: String,
    pub transformation: TransformationType,
    pub sources: Vec<ColumnSource>,
}

/// Classification of the transformation applied to produce an output column.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TransformationType {
    /// Direct column reference or rename (e.g. `SELECT id AS order_id`)
    Direct,
    /// Aggregate function (e.g. `COUNT(*)`, `SUM(amount)`)
    Aggregation,
    /// Arithmetic or other expression (e.g. `price * quantity`)
    Expression,
    /// Type cast (e.g. `CAST(x AS INT)`)
    Cast,
    /// Conditional expression (e.g. `CASE WHEN ...`)
    Conditional,
    /// Could not classify the transformation
    Unknown,
}

/// A source column reference
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ColumnSource {
    /// Source table/model name as it appears in SQL (e.g. "stg_orders", "`raw`.`orders`")
    pub table: String,
    /// Source column name
    pub column: String,
    /// Cross-model path: intermediate model names traversed to reach this source
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_path: Vec<String>,
}

// --- Column lineage disk cache ---

const COLUMN_LINEAGE_CACHE_FILENAME: &str = "column_lineage_cache.json";
const CACHE_DIR: &str = ".dlin_cache";

/// A single cached column lineage entry for one model
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ColumnLineageCacheEntry {
    /// FNV-1a hash of the model's compiled SQL
    compiled_code_hash: u64,
    /// Dialect used for parsing (e.g. "bigquery", "generic")
    dialect: String,
    /// FNV-1a hash covering the model's YAML columns, compiled SQL, and the same
    /// for all transitive upstream dependencies. Captures any schema or SQL change
    /// that could alter the lineage result, not just manifest column definitions.
    /// Defaults to 0 for cache entries created before this field was added,
    /// which effectively invalidates them since the computed hash will differ.
    #[serde(default)]
    manifest_columns_hash: u64,
    /// Cached lineage result
    lineage: ModelColumnLineage,
}

/// On-disk cache file structure
#[derive(Debug, Serialize, Deserialize)]
struct ColumnLineageCacheFile {
    /// dlin version that created this cache
    #[serde(default)]
    version: String,
    /// Per-model cached entries keyed by model name
    entries: HashMap<String, ColumnLineageCacheEntry>,
}

/// In-memory cache for column lineage results that can be loaded from and saved to disk
pub struct ColumnLineageCache {
    version: String,
    entries: HashMap<String, ColumnLineageCacheEntry>,
    /// `None` when the cache is disabled (no-op mode).
    cache_path: Option<PathBuf>,
    dirty: bool,
}

impl ColumnLineageCache {
    /// Create a no-op cache that never reads from or writes to disk.
    pub fn disabled() -> Self {
        Self {
            version: String::new(),
            entries: HashMap::new(),
            cache_path: None,
            dirty: false,
        }
    }

    /// Load the cache from disk, or create an empty one.
    /// Entries are discarded when the dlin version doesn't match.
    pub fn load(project_dir: &Path, cache_dir: Option<&Path>) -> Self {
        let cache_path = match cache_dir {
            Some(dir) => dir.join(COLUMN_LINEAGE_CACHE_FILENAME),
            None => project_dir
                .join(CACHE_DIR)
                .join(COLUMN_LINEAGE_CACHE_FILENAME),
        };
        let version = env!("CARGO_PKG_VERSION").to_string();

        let entries = std::fs::read_to_string(&cache_path)
            .ok()
            .and_then(|content| serde_json::from_str::<ColumnLineageCacheFile>(&content).ok())
            .filter(|cf| cf.version == version)
            .map(|cf| cf.entries)
            .unwrap_or_default();

        Self {
            version,
            entries,
            cache_path: Some(cache_path),
            dirty: false,
        }
    }

    /// Create an empty cache that ignores existing on-disk entries but
    /// still writes results to disk on [`save`](Self::save).
    pub fn fresh(project_dir: &Path, cache_dir: Option<&Path>) -> Self {
        let cache_path = match cache_dir {
            Some(dir) => dir.join(COLUMN_LINEAGE_CACHE_FILENAME),
            None => project_dir
                .join(CACHE_DIR)
                .join(COLUMN_LINEAGE_CACHE_FILENAME),
        };
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            entries: HashMap::new(),
            cache_path: Some(cache_path),
            dirty: false,
        }
    }

    /// Look up a cached lineage result for the given model.
    /// Returns `None` if not cached or if compiled_code/dialect/manifest_columns_hash have changed.
    pub fn get(
        &self,
        model_name: &str,
        compiled_code: &str,
        dialect: DialectType,
        manifest_columns_hash: u64,
    ) -> Option<&ModelColumnLineage> {
        let entry = self.entries.get(model_name)?;
        let code_hash = hash_str(compiled_code);
        let dialect_str = format!("{:?}", dialect);
        if entry.compiled_code_hash == code_hash
            && entry.dialect == dialect_str
            && entry.manifest_columns_hash == manifest_columns_hash
        {
            Some(&entry.lineage)
        } else {
            None
        }
    }

    /// Insert a lineage result into the cache.
    pub fn insert(
        &mut self,
        model_name: &str,
        compiled_code: &str,
        dialect: DialectType,
        manifest_columns_hash: u64,
        lineage: ModelColumnLineage,
    ) {
        self.entries.insert(
            model_name.to_string(),
            ColumnLineageCacheEntry {
                compiled_code_hash: hash_str(compiled_code),
                dialect: format!("{:?}", dialect),
                manifest_columns_hash,
                lineage,
            },
        );
        self.dirty = true;
    }

    /// Save the cache to disk if it has been modified.
    pub fn save(&self) {
        let cache_path = match (&self.cache_path, self.dirty) {
            (Some(p), true) => p,
            _ => return,
        };
        let cf = ColumnLineageCacheFile {
            version: self.version.clone(),
            entries: self.entries.clone(),
        };
        if let Some(parent) = cache_path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                crate::warn!("could not create cache directory: {}", parent.display());
                return;
            }
            // Auto-create .gitignore to prevent accidental commits
            let gitignore = parent.join(".gitignore");
            if !gitignore.exists()
                && let Err(e) = std::fs::write(&gitignore, "# Automatically created by dlin\n*\n")
            {
                crate::warn!("could not create {}: {}", gitignore.display(), e);
            }
        }
        match serde_json::to_string(&cf) {
            Ok(json) => {
                if let Err(e) = std::fs::write(cache_path, json) {
                    crate::warn!("could not write cache file {}: {}", cache_path.display(), e);
                }
            }
            Err(e) => {
                crate::warn!("could not serialize column lineage cache: {}", e);
            }
        }
    }
}

/// Column impact result for a single model+column pair
#[derive(Debug, Serialize)]
pub struct ColumnImpactReport {
    pub model: String,
    pub column: String,
    pub impacted_columns: Vec<ImpactedColumn>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// A downstream column affected by a change to the source column
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ImpactedColumn {
    /// Downstream model name
    pub model: String,
    /// Downstream column name
    pub column: String,
    /// How the downstream column uses this source
    pub transformation: TransformationType,
    /// Models traversed from source to this downstream column
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub model_path: Vec<String>,
}

/// Compute column-level lineage for a model using polyglot-sql.
///
/// Takes the manifest and a model name (short label like "orders"),
/// and returns the column lineage for that model.
pub fn compute_column_lineage(
    manifest: &Manifest,
    model_name: &str,
    dialect: DialectType,
    cache: &mut ColumnLineageCache,
) -> ModelColumnLineage {
    // Find the node in the manifest
    let node = manifest
        .nodes
        .values()
        .find(|n| n.name == model_name && n.resource_type == "model");

    let node = match node {
        Some(n) => n,
        None => {
            return ModelColumnLineage {
                model: model_name.to_string(),
                traced_columns: 0,
                total_columns: 0,
                columns: vec![],
                errors: vec![format!("model '{}' not found in manifest", model_name)],
            };
        }
    };

    let compiled_code = match &node.compiled_code {
        Some(code) => code,
        None => {
            return ModelColumnLineage {
                model: model_name.to_string(),
                traced_columns: 0,
                total_columns: 0,
                columns: vec![],
                errors: vec![format!(
                    "model '{}' has no compiled_code (run `dbt compile` first)",
                    model_name
                )],
            };
        }
    };

    // Compute YAML hash for cache key (own columns + upstream deps' YAML columns)
    let manifest_columns_hash = compute_manifest_columns_hash(manifest, node);

    // Check disk cache
    if let Some(cached) = cache.get(model_name, compiled_code, dialect, manifest_columns_hash) {
        return cached.clone();
    }

    // Get column names: union of YAML-defined and SQL-inferred columns so that
    // partially documented models include undocumented SQL output columns.
    let column_names: Vec<String> = {
        let mut names: HashSet<String> = node.columns.keys().cloned().collect();
        let schema = build_yaml_schema_for_node(manifest, node);
        names.extend(infer_output_columns(
            compiled_code,
            dialect,
            schema.as_ref(),
        ));
        let mut names: Vec<String> = names.into_iter().collect();
        names.sort();
        names
    };

    if column_names.is_empty() {
        return ModelColumnLineage {
            model: model_name.to_string(),
            traced_columns: 0,
            total_columns: 0,
            columns: vec![],
            errors: vec![format!(
                "model '{}': could not determine output columns (YAML has no columns and SQL inference failed)",
                model_name
            )],
        };
    }

    let ctx = match prepare_lineage_context(compiled_code, manifest, node, dialect) {
        Ok(ctx) => ctx,
        Err(e) => {
            return ModelColumnLineage {
                model: model_name.to_string(),
                traced_columns: 0,
                total_columns: column_names.len(),
                columns: vec![],
                errors: vec![format!("failed to parse SQL for '{}': {}", model_name, e)],
            };
        }
    };

    let total = column_names.len();

    let results: Vec<_> = column_names
        .par_iter()
        .map(|col_name| match run_column_lineage(col_name, &ctx) {
            Ok(result) => Ok(ColumnLineageEntry {
                column: col_name.clone(),
                transformation: result.transformation,
                sources: result.sources,
            }),
            Err(e) => Err(format!("column '{}': {}", col_name, e)),
        })
        .collect();

    let mut columns = Vec::new();
    let mut errors = Vec::new();
    for result in results {
        match result {
            Ok(entry) => columns.push(entry),
            Err(e) => errors.push(e),
        }
    }

    // Add summary when some columns failed but not all
    let failed = total - columns.len();
    if failed > 0 && !columns.is_empty() {
        errors.insert(
            0,
            format!(
                "model '{}': traced {}/{} columns ({} failed)",
                model_name,
                columns.len(),
                total,
                failed
            ),
        );
    }

    let result = ModelColumnLineage {
        model: model_name.to_string(),
        traced_columns: columns.len(),
        total_columns: total,
        columns,
        errors,
    };

    // Store in disk cache
    cache.insert(
        model_name,
        compiled_code,
        dialect,
        manifest_columns_hash,
        result.clone(),
    );

    result
}

/// Prepared context for running column lineage on a model's SQL.
struct LineageContext {
    expanded_expr: Expression,
    schema: Option<polyglot_sql::MappingSchema>,
}

/// Parse compiled SQL, build schema from manifest, and expand CTE stars.
fn prepare_lineage_context(
    compiled_code: &str,
    manifest: &Manifest,
    node: &crate::parser::manifest::ManifestNode,
    dialect: DialectType,
) -> Result<LineageContext, String> {
    let expr = polyglot_sql::parse_one(compiled_code, dialect).map_err(|e| format!("{}", e))?;

    let schema = build_schema_from_manifest(manifest, node, dialect);

    // Pre-expand CTE stars using schema for external table column lookup.
    // This is done before lineage() because qualify_columns may fail on complex
    // CTEs with ambiguous column references.
    let mut expanded_expr = expr;
    polyglot_sql::lineage::expand_cte_stars(
        &mut expanded_expr,
        schema.as_ref().map(|s| s as &dyn polyglot_sql::Schema),
    );

    Ok(LineageContext {
        expanded_expr,
        schema,
    })
}

/// Run lineage analysis for a single column on a prepared context.
///
/// When schema is available, prefers lineage_with_schema for proper table name
/// resolution (aliases like "o" are resolved to actual table names like "stg_orders").
/// Falls back to lineage without schema if lineage_with_schema fails.
/// The schema-first order is intentional: cross-model lineage needs table name
/// resolution that only works with schema. The polyglot-sql MAX_LINEAGE_DEPTH=64
/// limit mitigates the qualify_columns stack overflow risk.
fn run_column_lineage(col_name: &str, ctx: &LineageContext) -> Result<ColumnLineageResult, String> {
    let lineage_result = if let Some(ref s) = ctx.schema {
        polyglot_sql::lineage::lineage_with_schema(
            col_name,
            &ctx.expanded_expr,
            Some(s as &dyn polyglot_sql::Schema),
            None,
            false,
        )
        .or_else(|_| polyglot_sql::lineage::lineage(col_name, &ctx.expanded_expr, None, false))
    } else {
        polyglot_sql::lineage::lineage(col_name, &ctx.expanded_expr, None, false)
    };

    match lineage_result {
        Ok(node) => Ok(extract_leaf_sources(&node)),
        Err(e) => Err(format_lineage_error(&e)),
    }
}

/// Format a polyglot-sql error into a user-friendly message.
///
/// Strips meaningless position info ("at line 0, column 0") that polyglot-sql
/// emits for lineage errors where no source position is available.
fn format_lineage_error(e: &polyglot_sql::Error) -> String {
    let msg = e.to_string();
    // polyglot-sql lineage errors use line=0, column=0 as placeholder values.
    // "Parse error at line 0, column 0: Cannot find column 'x'" →
    // "lineage failed: Cannot find column 'x'"
    if let Some(rest) = msg
        .strip_prefix("Parse error at line 0, column 0: ")
        .or_else(|| msg.strip_prefix("Syntax error at line 0, column 0: "))
    {
        format!("lineage failed: {}", rest)
    } else if msg.starts_with("Internal error: ") {
        // "Internal error: lineage recursion depth exceeded..." →
        // "lineage failed: recursion depth exceeded..."
        format!(
            "lineage failed: {}",
            msg.strip_prefix("Internal error: ").unwrap()
        )
    } else {
        msg
    }
}

/// Compute column-level lineage with cross-model chain tracking.
///
/// Like `compute_column_lineage`, but recursively follows source references
/// through upstream models until reaching dbt source tables (raw tables).
///
/// For example, if `orders.total_amount` traces to `stg_payments.amount`,
/// and `stg_payments.amount` traces to `raw.payments.amount`, the final result
/// will show `raw.payments.amount` as the ultimate source.
pub fn compute_cross_model_column_lineage(
    manifest: &Manifest,
    model_name: &str,
    dialect: DialectType,
    cache: &mut ColumnLineageCache,
) -> ModelColumnLineage {
    // Track models currently being computed to detect cycles (A → B → A).
    // This is separate from the per-column visited set which prevents
    // self-references within a single resolution path.
    let mut ctx = CrossModelContext {
        manifest,
        dialect,
        in_memory_cache: HashMap::new(),
        computing: HashSet::new(),
    };
    ctx.computing.insert(model_name.to_string());
    compute_cross_model_inner(model_name, &mut ctx, cache)
}

/// Compute downstream column-level impact for a specific model+column.
///
/// Traces through the DAG to find all downstream columns that depend on the
/// specified column, one hop at a time.
pub fn compute_column_impact(
    manifest: &Manifest,
    model_name: &str,
    column_name: &str,
    dialect: DialectType,
    cache: &mut ColumnLineageCache,
) -> ColumnImpactReport {
    // Verify the model exists
    let model_exists = manifest
        .nodes
        .values()
        .any(|n| n.name == model_name && n.resource_type == "model");
    if !model_exists {
        return ColumnImpactReport {
            model: model_name.to_string(),
            column: column_name.to_string(),
            impacted_columns: vec![],
            errors: vec![format!("model '{}' not found in manifest", model_name)],
        };
    }

    // Build reverse dependency map: model_name → list of downstream model names
    let downstream_map = build_downstream_model_map(manifest);

    let mut impacted = Vec::new();
    let mut errors = Vec::new();
    // Track (model, column) pairs to avoid re-processing the same column through
    // the same model, while still allowing different columns to flow through a
    // shared intermediate model independently.
    let mut visited: HashSet<(String, String)> = HashSet::new();
    visited.insert((model_name.to_string(), column_name.to_string()));
    // Cache lineage results per model to avoid redundant compute_column_lineage calls
    // when the same downstream model appears as a dependent of multiple queue items.
    let mut lineage_cache: HashMap<String, ModelColumnLineage> = HashMap::new();

    // Seed: find direct dependents of the target model
    // and check which of their columns reference target model+column
    let mut queue: Vec<(String, String, Vec<String>)> =
        vec![(model_name.to_string(), column_name.to_string(), vec![])];

    while let Some((source_model, source_column, current_path)) = queue.pop() {
        let dependents = match downstream_map.get(&source_model) {
            Some(deps) => deps,
            None => continue,
        };

        for dep_model in dependents {
            let lineage = lineage_cache
                .entry(dep_model.clone())
                .or_insert_with(|| compute_column_lineage(manifest, dep_model, dialect, cache));
            for err in &lineage.errors {
                if !errors.contains(err) {
                    errors.push(err.clone());
                }
            }

            for entry in &lineage.columns {
                let pair = (dep_model.clone(), entry.column.clone());
                if visited.contains(&pair) {
                    continue;
                }

                // Check if any source of this column references the source model+column
                let references_source = entry.sources.iter().any(|s| {
                    let table_matches =
                        s.table == source_model || normalize_table_name(&s.table) == source_model;
                    table_matches && s.column == source_column
                });

                if references_source {
                    visited.insert(pair);

                    let mut path = current_path.clone();
                    path.push(dep_model.clone());

                    impacted.push(ImpactedColumn {
                        model: dep_model.clone(),
                        column: entry.column.clone(),
                        transformation: entry.transformation.clone(),
                        model_path: path.clone(),
                    });

                    // Enqueue for further downstream tracing
                    queue.push((dep_model.clone(), entry.column.clone(), path));
                }
            }
        }
    }

    // Sort for deterministic output
    impacted.sort_by(|a, b| (&a.model, &a.column).cmp(&(&b.model, &b.column)));
    impacted.dedup();

    ColumnImpactReport {
        model: model_name.to_string(),
        column: column_name.to_string(),
        impacted_columns: impacted,
        errors,
    }
}

/// Build a mapping from model name → list of downstream model names.
///
/// This is the reverse of depends_on: for each model that depends on X,
/// X maps to that model.
fn build_downstream_model_map(manifest: &Manifest) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();

    for node in manifest.nodes.values() {
        if node.resource_type != "model" {
            continue;
        }
        for dep_id in &node.depends_on.nodes {
            if let Some(dep_node) = manifest.nodes.get(dep_id)
                && dep_node.resource_type == "model"
            {
                map.entry(dep_node.name.clone())
                    .or_default()
                    .push(node.name.clone());
            }
        }
    }

    map
}

/// Shared context for cross-model resolution to avoid passing many arguments.
struct CrossModelContext<'a> {
    manifest: &'a Manifest,
    dialect: DialectType,
    in_memory_cache: HashMap<String, ModelColumnLineage>,
    computing: HashSet<String>,
}

fn compute_cross_model_inner(
    model_name: &str,
    ctx: &mut CrossModelContext<'_>,
    disk_cache: &mut ColumnLineageCache,
) -> ModelColumnLineage {
    // Compute single-model lineage first
    let mut result = compute_column_lineage(ctx.manifest, model_name, ctx.dialect, disk_cache);

    // Build a mapping: table name (as appears in SQL output) → model name
    // for the current model's upstream dependencies
    let upstream_models = build_upstream_model_names(ctx.manifest, model_name);

    // For each column, resolve sources through upstream models.
    // Each column gets its own visited set to avoid cross-column interference.
    // Tracks (model, column) pairs so different columns through a shared upstream
    // model are independently resolved.
    for entry in &mut result.columns {
        let mut resolved_sources = Vec::new();
        let mut visited: HashSet<(String, String)> = HashSet::new();
        visited.insert((model_name.to_string(), entry.column.clone()));

        for source in &entry.sources {
            resolve_source_recursive(
                source,
                &upstream_models,
                &mut visited,
                &mut resolved_sources,
                &mut result.errors,
                ctx,
                disk_cache,
                &[],
            );
        }

        // Deduplicate and sort
        resolved_sources.sort_by(|a, b| (&a.table, &a.column).cmp(&(&b.table, &b.column)));
        resolved_sources.dedup();
        entry.sources = resolved_sources;
    }

    result
}

/// Build a mapping from table names (as they may appear in SQL lineage output)
/// to model names for upstream model dependencies.
fn build_upstream_model_names(manifest: &Manifest, model_name: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();

    let node = manifest
        .nodes
        .values()
        .find(|n| n.name == model_name && n.resource_type == "model");

    let node = match node {
        Some(n) => n,
        None => return map,
    };

    for dep_id in &node.depends_on.nodes {
        if let Some(dep_node) = manifest.nodes.get(dep_id) {
            if dep_node.resource_type != "model" {
                continue;
            }
            // Register short name
            map.insert(dep_node.name.clone(), dep_node.name.clone());
            // Register FQ name (database.schema.name)
            let fq = make_fq_table_name(
                dep_node.database.as_deref(),
                dep_node.schema.as_deref(),
                &dep_node.name,
            );
            if fq != dep_node.name {
                map.insert(fq, dep_node.name.clone());
            }
        }
    }

    map
}

/// Normalize a table name by stripping quotes and extracting the short name.
///
/// Handles patterns like:
/// - `"jaffle_shop"."main"."stg_orders"` → `stg_orders`
/// - `` `raw`.`orders` `` → `orders`
/// - `stg_orders` → `stg_orders`
fn normalize_table_name(table: &str) -> String {
    let stripped: String = table.chars().filter(|c| *c != '"' && *c != '`').collect();
    stripped.rsplit('.').next().unwrap_or(&stripped).to_string()
}

#[allow(clippy::too_many_arguments)]
fn resolve_source_recursive(
    source: &ColumnSource,
    upstream_models: &HashMap<String, String>,
    visited: &mut HashSet<(String, String)>,
    resolved: &mut Vec<ColumnSource>,
    errors: &mut Vec<String>,
    ctx: &mut CrossModelContext<'_>,
    disk_cache: &mut ColumnLineageCache,
    current_path: &[String],
) {
    // Check if the source table matches an upstream model
    let model_name = upstream_models
        .get(&source.table)
        .or_else(|| {
            // Try normalized name (strip quotes, take last component)
            let normalized = normalize_table_name(&source.table);
            upstream_models.get(&normalized)
        })
        .cloned();

    let model_name = match model_name {
        Some(name) => {
            let pair = (name.clone(), source.column.clone());
            if visited.contains(&pair) {
                // Already visited this (model, column) — treat as leaf
                let mut leaf = source.clone();
                leaf.model_path = current_path.to_vec();
                resolved.push(leaf);
                return;
            }
            // Mark as visited to prevent re-entry for the same (model, column) pair
            // in diamond dependencies. Different columns through the same model are
            // resolved independently.
            visited.insert(pair);
            name
        }
        None => {
            // Source is a raw table or dbt source — leaf
            let mut leaf = source.clone();
            leaf.model_path = current_path.to_vec();
            resolved.push(leaf);
            return;
        }
    };

    // Extend the path with the current upstream model
    let mut extended_path = current_path.to_vec();
    extended_path.push(model_name.clone());

    // Get or compute the upstream model's lineage
    if !ctx.in_memory_cache.contains_key(&model_name) {
        if ctx.computing.contains(&model_name) {
            // Cycle detected (A → B → A) — treat as leaf
            let mut leaf = source.clone();
            leaf.model_path = current_path.to_vec();
            resolved.push(leaf);
            return;
        }
        ctx.computing.insert(model_name.clone());
        let upstream_result = compute_cross_model_inner(&model_name, ctx, disk_cache);
        ctx.in_memory_cache
            .insert(model_name.clone(), upstream_result);
    }
    let upstream_result = ctx.in_memory_cache.get(&model_name).unwrap();

    // Propagate upstream errors
    for err in &upstream_result.errors {
        if !errors.contains(err) {
            errors.push(err.clone());
        }
    }

    // Find the matching column in the upstream model's lineage
    if let Some(col_entry) = upstream_result
        .columns
        .iter()
        .find(|c| c.column == source.column)
    {
        if col_entry.sources.is_empty() {
            // Upstream column has no sources — keep original
            let mut leaf = source.clone();
            leaf.model_path = extended_path;
            resolved.push(leaf);
        } else {
            // The upstream's sources are already fully resolved (cross-model).
            // Prepend our path to each resolved source's model_path.
            for s in &col_entry.sources {
                let mut merged = s.clone();
                let mut full_path = extended_path.clone();
                full_path.extend(s.model_path.iter().cloned());
                merged.model_path = full_path;
                resolved.push(merged);
            }
        }
    } else {
        // Column not in precomputed lineage (e.g. not in YAML columns).
        // Try on-demand single-column lineage from the upstream model's SQL.
        let on_demand =
            compute_single_column_lineage(ctx.manifest, &model_name, &source.column, ctx.dialect);
        if on_demand.is_empty() {
            // Cannot resolve — keep as leaf
            let mut leaf = source.clone();
            leaf.model_path = extended_path;
            resolved.push(leaf);
        } else {
            // Recursively resolve the on-demand results through further upstream models
            let further_upstream = build_upstream_model_names(ctx.manifest, &model_name);
            for s in &on_demand {
                resolve_source_recursive(
                    s,
                    &further_upstream,
                    visited,
                    resolved,
                    errors,
                    ctx,
                    disk_cache,
                    &extended_path,
                );
            }
        }
    }
}

/// Compute lineage for a single column from a model's compiled SQL.
///
/// Used when the column isn't in the model's YAML-defined columns but exists
/// in the SQL output (common in dbt projects with incomplete column documentation).
fn compute_single_column_lineage(
    manifest: &Manifest,
    model_name: &str,
    column_name: &str,
    dialect: DialectType,
) -> Vec<ColumnSource> {
    let node = manifest
        .nodes
        .values()
        .find(|n| n.name == model_name && n.resource_type == "model");

    let node = match node {
        Some(n) => n,
        None => return vec![],
    };

    let compiled_code = match &node.compiled_code {
        Some(code) => code,
        None => return vec![],
    };

    let ctx = match prepare_lineage_context(compiled_code, manifest, node, dialect) {
        Ok(ctx) => ctx,
        Err(_) => return vec![],
    };

    run_column_lineage(column_name, &ctx)
        .map(|r| r.sources)
        .unwrap_or_default()
}

/// Build a MappingSchema from the manifest's upstream nodes for column qualification.
///
/// For each upstream dependency, columns are determined by:
/// 1. YAML-defined columns in the manifest (preferred)
/// 2. Inferring output columns from the upstream model's compiled SQL (fallback)
///
/// Tables are registered with their fully-qualified name (database.schema.name)
/// when database/schema info is available, so that references like
/// `"jaffle_shop"."main"."stg_orders"` can be resolved.
fn build_schema_from_manifest(
    manifest: &Manifest,
    node: &crate::parser::manifest::ManifestNode,
    dialect: DialectType,
) -> Option<polyglot_sql::MappingSchema> {
    let mut schema = polyglot_sql::MappingSchema::new();
    let mut has_entries = false;

    // Add columns from upstream dependencies
    for dep_id in &node.depends_on.nodes {
        // Try as a node (model/seed/snapshot)
        if let Some(dep_node) = manifest.nodes.get(dep_id) {
            let col_names = resolve_node_columns(dep_node, manifest, dialect);
            if !col_names.is_empty() {
                let cols: Vec<(String, polyglot_sql::expressions::DataType)> = col_names
                    .iter()
                    .map(|name| (name.clone(), polyglot_sql::expressions::DataType::Unknown))
                    .collect();

                // Register with fully-qualified name if database/schema available
                let fq_name = make_fq_table_name(
                    dep_node.database.as_deref(),
                    dep_node.schema.as_deref(),
                    &dep_node.name,
                );
                if schema.add_table(&fq_name, &cols, None).is_ok() {
                    has_entries = true;
                }
                // Also register with short name for non-qualified references
                if fq_name != dep_node.name {
                    let _ = schema.add_table(&dep_node.name, &cols, None);
                }
            }
            continue;
        }

        // Try as a source
        if let Some(dep_source) = manifest.sources.get(dep_id)
            && !dep_source.columns.is_empty()
        {
            let cols: Vec<(String, polyglot_sql::expressions::DataType)> = dep_source
                .columns
                .keys()
                .map(|name| (name.clone(), polyglot_sql::expressions::DataType::Unknown))
                .collect();
            let physical_identifier = dep_source.identifier.as_deref().unwrap_or(&dep_source.name);
            // Primary: physical FQ name (e.g. "mydb.raw.accounts")
            let physical_fq = make_fq_table_name(
                dep_source.database.as_deref(),
                dep_source.schema.as_deref(),
                physical_identifier,
            );
            if schema.add_table(&physical_fq, &cols, None).is_ok() {
                has_entries = true;
            }
            // Fallback aliases
            for alias in [
                physical_identifier,
                dep_source.name.as_str(),
                &format!("{}.{}", dep_source.source_name, dep_source.name),
            ] {
                if alias != physical_fq {
                    let _ = schema.add_table(alias, &cols, None);
                }
            }
        }
    }

    if has_entries { Some(schema) } else { None }
}

/// Resolve columns for a manifest node.
///
/// Merges SQL-inferred columns with YAML-defined columns to produce the most
/// complete column list possible. SQL inference alone may miss columns when
/// upstream models use `SELECT *` that can't be expanded; YAML definitions
/// alone may be incomplete. The union of both sources maximizes the chance
/// that `expand_cte_stars` can fully resolve star expressions.
///
/// A YAML-only schema of the node's own dependencies is built and passed to
/// the SQL inference so that `SELECT * FROM <upstream>` can be expanded when
/// the upstream's columns are defined in YAML. Only YAML columns are used for
/// the schema to avoid recursion (`build_schema_from_manifest` calls this
/// function, so calling it back would recurse).
fn resolve_node_columns(
    dep_node: &crate::parser::manifest::ManifestNode,
    manifest: &Manifest,
    dialect: DialectType,
) -> Vec<String> {
    let yaml_cols: HashSet<String> = dep_node.columns.keys().cloned().collect();
    let inferred_cols: HashSet<String> = dep_node
        .compiled_code
        .as_ref()
        .map(|code| {
            let schema = build_yaml_schema_for_node(manifest, dep_node);
            infer_output_columns(code, dialect, schema.as_ref())
        })
        .unwrap_or_default()
        .into_iter()
        .collect();

    let merged: Vec<String> = yaml_cols.union(&inferred_cols).cloned().collect();
    merged
}

/// Build a lightweight schema from YAML-defined columns of a node's direct dependencies.
///
/// Unlike `build_schema_from_manifest`, this does NOT call `resolve_node_columns`
/// (which would cause recursion). It only uses YAML columns from the manifest,
/// providing enough info for `expand_cte_stars` to resolve `SELECT *` when
/// upstream tables have YAML column definitions.
fn build_yaml_schema_for_node(
    manifest: &Manifest,
    node: &crate::parser::manifest::ManifestNode,
) -> Option<polyglot_sql::MappingSchema> {
    let mut schema = polyglot_sql::MappingSchema::new();
    let mut has_entries = false;

    for dep_id in &node.depends_on.nodes {
        if let Some(dep_node) = manifest.nodes.get(dep_id) {
            if !dep_node.columns.is_empty() {
                let cols: Vec<(String, polyglot_sql::expressions::DataType)> = dep_node
                    .columns
                    .keys()
                    .map(|name| (name.clone(), polyglot_sql::expressions::DataType::Unknown))
                    .collect();
                let fq_name = make_fq_table_name(
                    dep_node.database.as_deref(),
                    dep_node.schema.as_deref(),
                    &dep_node.name,
                );
                if schema.add_table(&fq_name, &cols, None).is_ok() {
                    has_entries = true;
                }
                if fq_name != dep_node.name {
                    let _ = schema.add_table(&dep_node.name, &cols, None);
                }
            }
            continue;
        }

        if let Some(dep_source) = manifest.sources.get(dep_id)
            && !dep_source.columns.is_empty()
        {
            let cols: Vec<(String, polyglot_sql::expressions::DataType)> = dep_source
                .columns
                .keys()
                .map(|name| (name.clone(), polyglot_sql::expressions::DataType::Unknown))
                .collect();
            let physical_identifier = dep_source.identifier.as_deref().unwrap_or(&dep_source.name);
            // Primary: physical FQ name (e.g. "mydb.raw.accounts")
            let physical_fq = make_fq_table_name(
                dep_source.database.as_deref(),
                dep_source.schema.as_deref(),
                physical_identifier,
            );
            if schema.add_table(&physical_fq, &cols, None).is_ok() {
                has_entries = true;
            }
            // Fallback aliases
            for alias in [
                physical_identifier,
                dep_source.name.as_str(),
                &format!("{}.{}", dep_source.source_name, dep_source.name),
            ] {
                if alias != physical_fq {
                    let _ = schema.add_table(alias, &cols, None);
                }
            }
        }
    }

    if has_entries { Some(schema) } else { None }
}

/// Infer output column names from a model's compiled SQL by parsing it and extracting
/// the top-level SELECT column list. Handles CTE patterns by using lineage's
/// expand_cte_stars logic. When a schema is provided, `SELECT *` from external
/// tables can be expanded using the schema's column information.
fn infer_output_columns(
    sql: &str,
    dialect: DialectType,
    schema: Option<&polyglot_sql::MappingSchema>,
) -> Vec<String> {
    let expr = match polyglot_sql::parse_one(sql, dialect) {
        Ok(e) => e,
        Err(_) => return vec![],
    };
    crate::parser::columns::extract_select_columns_from_expr(
        &expr,
        schema.map(|s| s as &dyn polyglot_sql::Schema),
    )
}

/// Compute a deterministic hash of manifest column definitions that affect lineage results.
///
/// Includes the model's own columns and those of its direct upstream
/// dependencies (nodes and sources) from manifest.json. Changes to any of
/// these invalidate the cache.
fn compute_manifest_columns_hash(
    manifest: &Manifest,
    node: &crate::parser::manifest::ManifestNode,
) -> u64 {
    let mut visited: HashSet<String> = HashSet::new();
    hash_node_columns_transitive(manifest, node, &mut visited)
}

/// Recursively hash a node's YAML columns and its transitive dependency inputs.
///
/// `visited` prevents infinite loops in case of cyclic dependency graphs.
/// Sources (leaves) are hashed by their column list only; intermediate nodes
/// are hashed recursively so that grandparent schema changes invalidate the
/// cache all the way up.
fn hash_node_columns_transitive(
    manifest: &Manifest,
    node: &crate::parser::manifest::ManifestNode,
    visited: &mut HashSet<String>,
) -> u64 {
    let mut parts: Vec<String> = Vec::new();

    // Own YAML columns (sorted for determinism)
    let mut own_cols: Vec<&String> = node.columns.keys().collect();
    own_cols.sort();
    for col in own_cols {
        parts.push(col.clone());
    }
    // Own compiled SQL — captures column changes not reflected in YAML
    if let Some(code) = &node.compiled_code {
        parts.push(format!("sql:{}", hash_str(code)));
    }
    parts.push("|".to_string()); // separator between own and upstream

    // Recursively hash upstream dependencies so that grandparent schema changes
    // (e.g. a new column added to a grandparent's SQL) also invalidate this cache.
    let mut dep_ids: Vec<&String> = node.depends_on.nodes.iter().collect();
    dep_ids.sort();
    for dep_id in dep_ids {
        parts.push(dep_id.clone());
        if visited.contains(dep_id) {
            // Already visited; skip to avoid infinite recursion in cyclic graphs
            continue;
        }
        visited.insert(dep_id.clone());
        if let Some(dep_node) = manifest.nodes.get(dep_id) {
            let dep_hash = hash_node_columns_transitive(manifest, dep_node, visited);
            parts.push(format!("node:{}", dep_hash));
        } else if let Some(dep_source) = manifest.sources.get(dep_id) {
            // Sources are leaves — hash their columns directly
            let mut cols: Vec<&String> = dep_source.columns.keys().collect();
            cols.sort();
            for col in cols {
                parts.push(col.clone());
            }
        }
    }

    hash_str(&parts.join("\0"))
}

/// Build a fully-qualified table name from optional database, schema, and table name.
fn make_fq_table_name(database: Option<&str>, schema: Option<&str>, name: &str) -> String {
    match (database, schema) {
        (Some(db), Some(s)) => format!("{}.{}.{}", db, s, name),
        (None, Some(s)) => format!("{}.{}", s, name),
        _ => name.to_string(),
    }
}

/// Classify the transformation applied by the root expression of a lineage node.
fn classify_transformation(node: &polyglot_sql::lineage::LineageNode) -> TransformationType {
    classify_expression(&node.expression)
}

fn classify_expression(expr: &polyglot_sql::Expression) -> TransformationType {
    use polyglot_sql::Expression;
    match expr {
        Expression::Column(_) | Expression::Identifier(_) => TransformationType::Direct,
        // Alias wraps the actual expression — classify the inner expression
        Expression::Alias(alias) => classify_expression(&alias.this),
        Expression::Count(_)
        | Expression::Sum(_)
        | Expression::Avg(_)
        | Expression::Min(_)
        | Expression::Max(_) => TransformationType::Aggregation,
        Expression::Cast(_) => TransformationType::Cast,
        Expression::Case(_) => TransformationType::Conditional,
        Expression::Add(_) | Expression::Sub(_) | Expression::Mul(_) | Expression::Div(_) => {
            TransformationType::Expression
        }
        Expression::Anonymous(_) | Expression::Coalesce(_) | Expression::NullIf(_) => {
            TransformationType::Expression
        }
        _ => TransformationType::Unknown,
    }
}

/// Result of extracting lineage from a single column.
struct ColumnLineageResult {
    sources: Vec<ColumnSource>,
    transformation: TransformationType,
}

/// Walk the lineage tree and extract leaf-level source columns plus transformation type.
fn extract_leaf_sources(node: &polyglot_sql::lineage::LineageNode) -> ColumnLineageResult {
    let transformation = classify_transformation(node);
    let mut sources = Vec::new();
    collect_leaves(node, &mut sources);
    // Deduplicate
    sources.sort_by(|a, b| (&a.table, &a.column).cmp(&(&b.table, &b.column)));
    sources.dedup();
    ColumnLineageResult {
        sources,
        transformation,
    }
}

fn collect_leaves(node: &polyglot_sql::lineage::LineageNode, sources: &mut Vec<ColumnSource>) {
    if node.downstream.is_empty() {
        // Leaf node — this is a source column
        let name = &node.name;
        // Name is typically "table.column" or just "column"
        if let Some((table, column)) = name.rsplit_once('.') {
            sources.push(ColumnSource {
                table: table.to_string(),
                column: column.to_string(),
                model_path: vec![],
            });
        } else {
            sources.push(ColumnSource {
                table: String::new(),
                column: name.to_string(),
                model_path: vec![],
            });
        }
    } else {
        for child in &node.downstream {
            collect_leaves(child, sources);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::manifest::{
        DependsOn, ManifestColumn, ManifestConfig, ManifestNode, ManifestSource,
    };

    /// Build a minimal manifest for testing column lineage.
    fn make_test_manifest() -> Manifest {
        let mut nodes = HashMap::new();

        // stg_orders: SELECT id as order_id, user_id as customer_id, order_date, status FROM raw.orders
        let mut stg_orders_cols = HashMap::new();
        for name in ["order_id", "customer_id", "order_date", "status"] {
            stg_orders_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        nodes.insert("model.proj.stg_orders".to_string(), ManifestNode {
            unique_id: "model.proj.stg_orders".to_string(),
            name: "stg_orders".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn { nodes: vec!["source.proj.raw.orders".to_string()] },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            columns: stg_orders_cols,
            compiled_code: Some("select id as order_id, user_id as customer_id, order_date, status from raw.orders".to_string()),
            database: None,
            schema: None,
        });

        // orders: SELECT o.order_id, o.customer_id, p.amount as total_amount FROM stg_orders o LEFT JOIN stg_payments p ON o.order_id = p.order_id
        let mut orders_cols = HashMap::new();
        for name in ["order_id", "customer_id", "total_amount"] {
            orders_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        nodes.insert("model.proj.orders".to_string(), ManifestNode {
            unique_id: "model.proj.orders".to_string(),
            name: "orders".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn { nodes: vec![
                "model.proj.stg_orders".to_string(),
                "model.proj.stg_payments".to_string(),
            ] },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            columns: orders_cols,
            compiled_code: Some("select o.order_id, o.customer_id, p.amount as total_amount from stg_orders o left join stg_payments p on o.order_id = p.order_id".to_string()),
            database: None,
            schema: None,
        });

        // stg_payments (upstream, for schema)
        let mut stg_payments_cols = HashMap::new();
        for name in ["payment_id", "order_id", "amount", "payment_method"] {
            stg_payments_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        nodes.insert(
            "model.proj.stg_payments".to_string(),
            ManifestNode {
                unique_id: "model.proj.stg_payments".to_string(),
                name: "stg_payments".to_string(),
                resource_type: "model".to_string(),
                depends_on: DependsOn { nodes: vec![] },
                config: ManifestConfig::default(),
                description: None,
                path: None,
                columns: stg_payments_cols,
                compiled_code: Some(
                    "select id as payment_id, order_id, amount, payment_method from raw.payments"
                        .to_string(),
                ),
                database: None,
                schema: None,
            },
        );

        // Source: raw.orders
        let mut source_cols = HashMap::new();
        for name in ["id", "user_id", "order_date", "status"] {
            source_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        let mut sources = HashMap::new();
        sources.insert(
            "source.proj.raw.orders".to_string(),
            ManifestSource {
                unique_id: "source.proj.raw.orders".to_string(),
                name: "orders".to_string(),
                source_name: "raw".to_string(),
                resource_type: "source".to_string(),
                description: None,
                path: None,
                columns: source_cols,
                database: None,
                schema: None,
                identifier: None,
            },
        );

        Manifest {
            nodes,
            sources,
            exposures: HashMap::new(),
        }
    }

    #[test]
    fn test_rename_detection() {
        let manifest = make_test_manifest();
        let result = compute_column_lineage(
            &manifest,
            "stg_orders",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        assert_eq!(result.model, "stg_orders");
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.columns.len(), 4);

        // order_id comes from orders.id (renamed)
        let order_id = result
            .columns
            .iter()
            .find(|c| c.column == "order_id")
            .unwrap();
        assert!(!order_id.sources.is_empty(), "order_id should have sources");
        assert_eq!(order_id.sources[0].column, "id");
        // Rename is classified as direct (the rename is evident from column name difference)
        assert_eq!(order_id.transformation, TransformationType::Direct);

        // customer_id comes from orders.user_id (renamed)
        let customer_id = result
            .columns
            .iter()
            .find(|c| c.column == "customer_id")
            .unwrap();
        assert_eq!(customer_id.sources[0].column, "user_id");
        assert_eq!(customer_id.transformation, TransformationType::Direct);
    }

    #[test]
    fn test_join_lineage() {
        let manifest = make_test_manifest();
        let result = compute_column_lineage(
            &manifest,
            "orders",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        assert_eq!(result.model, "orders");
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.columns.len(), 3);

        // total_amount is aliased from p.amount
        let total_amount = result
            .columns
            .iter()
            .find(|c| c.column == "total_amount")
            .unwrap();
        assert!(!total_amount.sources.is_empty());
        assert_eq!(total_amount.sources[0].column, "amount");

        // order_id comes from o.order_id
        let order_id = result
            .columns
            .iter()
            .find(|c| c.column == "order_id")
            .unwrap();
        assert_eq!(order_id.sources[0].column, "order_id");
    }

    #[test]
    fn test_model_not_found() {
        let manifest = make_test_manifest();
        let result = compute_column_lineage(
            &manifest,
            "nonexistent",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        assert_eq!(result.columns.len(), 0);
        assert!(!result.errors.is_empty());
        assert!(result.errors[0].contains("not found"));
    }

    #[test]
    fn test_no_compiled_code() {
        let mut manifest = make_test_manifest();
        // Remove compiled_code from stg_orders
        manifest
            .nodes
            .get_mut("model.proj.stg_orders")
            .unwrap()
            .compiled_code = None;
        let result = compute_column_lineage(
            &manifest,
            "stg_orders",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        assert!(result.columns.is_empty());
        assert!(result.errors[0].contains("compiled_code"));
    }

    #[test]
    fn test_no_yaml_columns_uses_sql_inference() {
        // When YAML columns are empty, column names should be inferred from compiled SQL
        let mut manifest = make_test_manifest();
        manifest
            .nodes
            .get_mut("model.proj.stg_orders")
            .unwrap()
            .columns
            .clear();
        let result = compute_column_lineage(
            &manifest,
            "stg_orders",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        // SQL inference should find: customer_id, order_date, order_id, status
        assert_eq!(
            result.columns.len(),
            4,
            "should infer 4 columns from SQL: {:?}",
            result.errors
        );
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    }

    #[test]
    fn test_no_columns_and_no_sql() {
        // When YAML columns are empty AND compiled SQL cannot be parsed, error
        let mut manifest = make_test_manifest();
        let node = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
        node.columns.clear();
        node.compiled_code = Some("INVALID SQL %%%".to_string());
        let result = compute_column_lineage(
            &manifest,
            "stg_orders",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        assert!(result.columns.is_empty());
        assert!(!result.errors.is_empty());
        assert!(result.errors[0].contains("could not determine output columns"));
    }

    #[test]
    fn test_cte_select_star() {
        // CTE + SELECT * now works with the expand_cte_stars preprocessing
        let sql = r#"with renamed as (select id as customer_id from source) select * from renamed"#;
        let expr = polyglot_sql::parse_one(sql, polyglot_sql::DialectType::Generic).unwrap();
        let result = polyglot_sql::lineage::lineage("customer_id", &expr, None, false);
        assert!(
            result.is_ok(),
            "CTE + SELECT * should work: {:?}",
            result.err()
        );
        let node = result.unwrap();
        assert_eq!(node.name, "customer_id");
    }

    #[test]
    fn test_nested_cte_select_star() {
        // Nested CTE: cte2 references cte1 via SELECT *
        let sql = r#"
            with
                cte1 as (select id as order_id, amount from raw_orders),
                cte2 as (select * from cte1)
            select * from cte2
        "#;
        let expr = polyglot_sql::parse_one(sql, polyglot_sql::DialectType::Generic).unwrap();
        let result = polyglot_sql::lineage::lineage("order_id", &expr, None, false);
        assert!(
            result.is_ok(),
            "nested CTE + SELECT * should work: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_cte_select_star_in_manifest_model() {
        // Integration test: typical dbt pattern with CTE + SELECT *
        let mut manifest = make_test_manifest();
        let sql = r#"with renamed as (
            select
                id as order_id,
                user_id as customer_id,
                order_date,
                status
            from raw.orders
        )
        select * from renamed"#;
        manifest
            .nodes
            .get_mut("model.proj.stg_orders")
            .unwrap()
            .compiled_code = Some(sql.to_string());
        let result = compute_column_lineage(
            &manifest,
            "stg_orders",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.columns.len(), 4);

        let order_id = result
            .columns
            .iter()
            .find(|c| c.column == "order_id")
            .unwrap();
        assert_eq!(order_id.sources[0].column, "id");
    }

    #[test]
    fn test_schema_resolves_cte_star_from_external_table() {
        // Test that lineage_with_schema can resolve columns through CTEs that
        // reference external tables registered in the schema.
        let sql = r#"with
orders as (
    select * from stg_orders
),
enriched as (
    select orders.*, 'extra' as extra_col
    from orders
)
select * from enriched"#;
        let expr = polyglot_sql::parse_one(sql, polyglot_sql::DialectType::Generic).unwrap();

        let mut schema = polyglot_sql::MappingSchema::new();
        let cols = vec![
            (
                "order_id".to_string(),
                polyglot_sql::expressions::DataType::Unknown,
            ),
            (
                "customer_id".to_string(),
                polyglot_sql::expressions::DataType::Unknown,
            ),
            (
                "order_total".to_string(),
                polyglot_sql::expressions::DataType::Unknown,
            ),
        ];
        schema.add_table("stg_orders", &cols, None).unwrap();

        let result = polyglot_sql::lineage::lineage_with_schema(
            "order_id",
            &expr,
            Some(&schema as &dyn polyglot_sql::Schema),
            None,
            false,
        );
        assert!(
            result.is_ok(),
            "should resolve order_id: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_schema_resolves_three_part_name() {
        // Test with fully-qualified 3-part table name as dbt generates
        let sql = r#"with
orders as (
    select * from "jaffle_shop"."main"."stg_orders"
)
select * from orders"#;
        let expr = polyglot_sql::parse_one(sql, polyglot_sql::DialectType::Generic).unwrap();

        let mut schema = polyglot_sql::MappingSchema::new();
        let cols = vec![
            (
                "order_id".to_string(),
                polyglot_sql::expressions::DataType::Unknown,
            ),
            (
                "customer_id".to_string(),
                polyglot_sql::expressions::DataType::Unknown,
            ),
        ];
        // Register with 3-part name
        schema
            .add_table("jaffle_shop.main.stg_orders", &cols, None)
            .unwrap();

        let result = polyglot_sql::lineage::lineage_with_schema(
            "order_id",
            &expr,
            Some(&schema as &dyn polyglot_sql::Schema),
            None,
            false,
        );
        assert!(
            result.is_ok(),
            "should resolve order_id via 3-part name: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_json_serialization() {
        let manifest = make_test_manifest();
        let result = compute_column_lineage(
            &manifest,
            "stg_orders",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );
        let json = serde_json::to_string_pretty(&result).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["model"], "stg_orders");
        assert!(parsed["columns"].is_array());
    }

    // --- Cross-model lineage tests ---

    /// Build a manifest with 3 levels: customers → orders → stg_orders → raw.orders
    fn make_cross_model_manifest() -> Manifest {
        let mut nodes = HashMap::new();

        // Source: raw.orders
        let mut raw_orders_cols = HashMap::new();
        for name in ["id", "user_id", "order_date", "status"] {
            raw_orders_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        let mut sources = HashMap::new();
        sources.insert(
            "source.proj.raw.orders".to_string(),
            ManifestSource {
                unique_id: "source.proj.raw.orders".to_string(),
                name: "orders".to_string(),
                source_name: "raw".to_string(),
                resource_type: "source".to_string(),
                description: None,
                path: None,
                columns: raw_orders_cols,
                database: None,
                schema: None,
                identifier: None,
            },
        );

        // Source: raw.payments
        let mut raw_payments_cols = HashMap::new();
        for name in ["id", "order_id", "amount", "payment_method"] {
            raw_payments_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        sources.insert(
            "source.proj.raw.payments".to_string(),
            ManifestSource {
                unique_id: "source.proj.raw.payments".to_string(),
                name: "payments".to_string(),
                source_name: "raw".to_string(),
                resource_type: "source".to_string(),
                description: None,
                path: None,
                columns: raw_payments_cols,
                database: None,
                schema: None,
                identifier: None,
            },
        );

        // stg_orders: renames id→order_id, user_id→customer_id
        let mut stg_orders_cols = HashMap::new();
        for name in ["order_id", "customer_id", "order_date", "status"] {
            stg_orders_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        nodes.insert(
            "model.proj.stg_orders".to_string(),
            ManifestNode {
                unique_id: "model.proj.stg_orders".to_string(),
                name: "stg_orders".to_string(),
                resource_type: "model".to_string(),
                depends_on: DependsOn {
                    nodes: vec!["source.proj.raw.orders".to_string()],
                },
                config: ManifestConfig::default(),
                description: None,
                path: None,
                columns: stg_orders_cols,
                compiled_code: Some(
                    "select id as order_id, user_id as customer_id, order_date, status from orders"
                        .to_string(),
                ),
                database: None,
                schema: None,
            },
        );

        // stg_payments: renames id→payment_id
        let mut stg_payments_cols = HashMap::new();
        for name in ["payment_id", "order_id", "amount", "payment_method"] {
            stg_payments_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        nodes.insert(
            "model.proj.stg_payments".to_string(),
            ManifestNode {
                unique_id: "model.proj.stg_payments".to_string(),
                name: "stg_payments".to_string(),
                resource_type: "model".to_string(),
                depends_on: DependsOn {
                    nodes: vec!["source.proj.raw.payments".to_string()],
                },
                config: ManifestConfig::default(),
                description: None,
                path: None,
                columns: stg_payments_cols,
                compiled_code: Some(
                    "select id as payment_id, order_id, amount, payment_method from payments"
                        .to_string(),
                ),
                database: None,
                schema: None,
            },
        );

        // orders: joins stg_orders + stg_payments (CTE pattern like real dbt compiled SQL)
        let mut orders_cols = HashMap::new();
        for name in ["order_id", "customer_id", "total_amount"] {
            orders_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        nodes.insert(
            "model.proj.orders".to_string(),
            ManifestNode {
                unique_id: "model.proj.orders".to_string(),
                name: "orders".to_string(),
                resource_type: "model".to_string(),
                depends_on: DependsOn {
                    nodes: vec![
                        "model.proj.stg_orders".to_string(),
                        "model.proj.stg_payments".to_string(),
                    ],
                },
                config: ManifestConfig::default(),
                description: None,
                path: None,
                columns: orders_cols,
                compiled_code: Some(
                    concat!(
                        "with stg_orders as (select * from stg_orders), ",
                        "stg_payments as (select * from stg_payments) ",
                        "select stg_orders.order_id, stg_orders.customer_id, ",
                        "stg_payments.amount as total_amount ",
                        "from stg_orders left join stg_payments ",
                        "on stg_orders.order_id = stg_payments.order_id"
                    )
                    .to_string(),
                ),
                database: None,
                schema: None,
            },
        );

        // customers: aggregates from orders model (CTE pattern)
        let mut customers_cols = HashMap::new();
        for name in ["customer_id", "order_count"] {
            customers_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        nodes.insert(
            "model.proj.customers".to_string(),
            ManifestNode {
                unique_id: "model.proj.customers".to_string(),
                name: "customers".to_string(),
                resource_type: "model".to_string(),
                depends_on: DependsOn {
                    nodes: vec!["model.proj.orders".to_string()],
                },
                config: ManifestConfig::default(),
                description: None,
                path: None,
                columns: customers_cols,
                compiled_code: Some(
                    concat!(
                "with orders as (select * from orders) ",
                "select customer_id, count(*) as order_count from orders group by customer_id"
            )
                    .to_string(),
                ),
                database: None,
                schema: None,
            },
        );

        Manifest {
            nodes,
            sources,
            exposures: HashMap::new(),
        }
    }

    #[test]
    fn test_cross_model_single_hop() {
        // orders.order_id → stg_orders.order_id → raw.orders.id
        let manifest = make_cross_model_manifest();
        let result = compute_cross_model_column_lineage(
            &manifest,
            "orders",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let order_id = result
            .columns
            .iter()
            .find(|c| c.column == "order_id")
            .unwrap();
        // Should trace through stg_orders to raw source (orders table)
        assert!(
            order_id
                .sources
                .iter()
                .any(|s| s.column == "id" && s.table.contains("orders")),
            "order_id should trace to raw orders.id, got: {:?}",
            order_id.sources
        );
        // model_path should show the hop through stg_orders
        let src = order_id.sources.iter().find(|s| s.column == "id").unwrap();
        assert!(
            src.model_path.contains(&"stg_orders".to_string()),
            "model_path should include stg_orders, got: {:?}",
            src.model_path
        );
    }

    #[test]
    fn test_cross_model_two_hops() {
        // customers.customer_id → orders.customer_id → stg_orders.customer_id → raw.orders.user_id
        let manifest = make_cross_model_manifest();
        let result = compute_cross_model_column_lineage(
            &manifest,
            "customers",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let customer_id = result
            .columns
            .iter()
            .find(|c| c.column == "customer_id")
            .unwrap();
        assert!(
            customer_id
                .sources
                .iter()
                .any(|s| s.column == "user_id" && s.table.contains("orders")),
            "customer_id should trace to raw orders.user_id, got: {:?}",
            customer_id.sources
        );
        // model_path should show both hops: orders → stg_orders
        let src = customer_id
            .sources
            .iter()
            .find(|s| s.column == "user_id")
            .unwrap();
        assert!(
            src.model_path.contains(&"orders".to_string())
                && src.model_path.contains(&"stg_orders".to_string()),
            "model_path should include orders and stg_orders, got: {:?}",
            src.model_path
        );
        // orders should come before stg_orders in the path
        let orders_pos = src.model_path.iter().position(|m| m == "orders").unwrap();
        let stg_pos = src
            .model_path
            .iter()
            .position(|m| m == "stg_orders")
            .unwrap();
        assert!(
            orders_pos < stg_pos,
            "orders should precede stg_orders in path"
        );
    }

    #[test]
    fn test_cross_model_join_sources() {
        // orders.total_amount → stg_payments.amount → raw.payments.amount
        let manifest = make_cross_model_manifest();
        let result = compute_cross_model_column_lineage(
            &manifest,
            "orders",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        let total_amount = result
            .columns
            .iter()
            .find(|c| c.column == "total_amount")
            .unwrap();
        assert!(
            total_amount
                .sources
                .iter()
                .any(|s| s.column == "amount" && s.table.contains("payments")),
            "total_amount should trace to raw payments.amount, got: {:?}",
            total_amount.sources
        );
        // model_path should show the hop through stg_payments
        let src = total_amount
            .sources
            .iter()
            .find(|s| s.column == "amount")
            .unwrap();
        assert!(
            src.model_path.contains(&"stg_payments".to_string()),
            "model_path should include stg_payments, got: {:?}",
            src.model_path
        );
    }

    #[test]
    fn test_cross_model_source_table_is_leaf() {
        // stg_orders directly references a source — cross-model should not change the result
        let manifest = make_cross_model_manifest();
        let single = compute_column_lineage(
            &manifest,
            "stg_orders",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );
        let cross = compute_cross_model_column_lineage(
            &manifest,
            "stg_orders",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        assert_eq!(single.columns.len(), cross.columns.len());
        for (s, c) in single.columns.iter().zip(cross.columns.iter()) {
            assert_eq!(s.column, c.column);
            assert_eq!(s.sources, c.sources);
        }
    }

    #[test]
    fn test_cross_model_model_not_found() {
        let manifest = make_cross_model_manifest();
        let result = compute_cross_model_column_lineage(
            &manifest,
            "nonexistent",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );
        assert!(!result.errors.is_empty());
        assert!(result.errors[0].contains("not found"));
    }

    #[test]
    fn test_normalize_table_name() {
        assert_eq!(normalize_table_name("stg_orders"), "stg_orders");
        assert_eq!(
            normalize_table_name("\"jaffle_shop\".\"main\".\"stg_orders\""),
            "stg_orders"
        );
        assert_eq!(normalize_table_name("`raw`.`orders`"), "orders");
        assert_eq!(normalize_table_name("schema.table"), "table");
    }

    #[test]
    fn test_format_lineage_error_strips_position() {
        let err = polyglot_sql::Error::parse("Cannot find column 'x' in query", 0, 0, 0, 0);
        let formatted = format_lineage_error(&err);
        assert_eq!(formatted, "lineage failed: Cannot find column 'x' in query");
        assert!(
            !formatted.contains("line 0"),
            "should strip meaningless position info"
        );
    }

    #[test]
    fn test_format_lineage_error_preserves_real_position() {
        let err = polyglot_sql::Error::parse("unexpected token", 5, 10, 0, 0);
        let formatted = format_lineage_error(&err);
        assert!(
            formatted.contains("line 5"),
            "should preserve real position info: {}",
            formatted
        );
    }

    #[test]
    fn test_format_lineage_error_internal() {
        let err = polyglot_sql::Error::internal("lineage recursion depth exceeded");
        let formatted = format_lineage_error(&err);
        assert_eq!(
            formatted,
            "lineage failed: lineage recursion depth exceeded"
        );
    }

    #[test]
    fn test_partial_failure_summary() {
        // Model with some columns that can be traced and some that fail
        let mut manifest = make_test_manifest();
        // Add a column to stg_orders that doesn't exist in the SQL
        let node = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
        node.columns.insert(
            "nonexistent_col".to_string(),
            ManifestColumn {
                name: "nonexistent_col".to_string(),
            },
        );
        let result = compute_column_lineage(
            &manifest,
            "stg_orders",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        // Should have 4 successful columns and 1 failed
        assert_eq!(result.columns.len(), 4);
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.contains("traced 4/5 columns (1 failed)")),
            "should include summary, got: {:?}",
            result.errors
        );
        assert!(
            result.errors.iter().any(|e| e.contains("nonexistent_col")),
            "should include per-column error, got: {:?}",
            result.errors
        );
    }

    #[test]
    fn test_transformation_classification() {
        // customers model has: customer_id (direct) and order_count (aggregation via count(*))
        let manifest = make_cross_model_manifest();
        let result = compute_cross_model_column_lineage(
            &manifest,
            "customers",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let customer_id = result
            .columns
            .iter()
            .find(|c| c.column == "customer_id")
            .unwrap();
        assert_eq!(
            customer_id.transformation,
            TransformationType::Direct,
            "customer_id should be direct"
        );

        let order_count = result
            .columns
            .iter()
            .find(|c| c.column == "order_count")
            .unwrap();
        assert_eq!(
            order_count.transformation,
            TransformationType::Aggregation,
            "order_count (count(*)) should be aggregation"
        );
    }

    #[test]
    fn test_source_table_has_empty_model_path() {
        // stg_orders references raw source directly — model_path should be empty
        let manifest = make_cross_model_manifest();
        let result = compute_cross_model_column_lineage(
            &manifest,
            "stg_orders",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        for entry in &result.columns {
            for source in &entry.sources {
                assert!(
                    source.model_path.is_empty(),
                    "source {}.{} should have empty model_path (no cross-model hops), got: {:?}",
                    source.table,
                    source.column,
                    source.model_path
                );
            }
        }
    }

    #[test]
    fn test_json_includes_new_fields() {
        let manifest = make_cross_model_manifest();
        let result = compute_cross_model_column_lineage(
            &manifest,
            "customers",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );
        let json = serde_json::to_string_pretty(&result).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // transformation field should be present on all columns
        for col in parsed["columns"].as_array().unwrap() {
            assert!(
                col["transformation"].is_string(),
                "transformation should be serialized: {:?}",
                col
            );
        }

        // model_path should be present on sources with cross-model hops
        let customer_id = parsed["columns"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["column"] == "customer_id")
            .unwrap();
        let first_source = &customer_id["sources"][0];
        assert!(
            first_source["model_path"].is_array(),
            "model_path should be present for cross-model source: {:?}",
            first_source
        );
    }

    #[test]
    fn test_traced_total_columns_success() {
        let manifest = make_test_manifest();
        let result = compute_column_lineage(
            &manifest,
            "stg_orders",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );
        assert_eq!(result.total_columns, 4);
        assert_eq!(result.traced_columns, 4);
    }

    #[test]
    fn test_traced_total_columns_partial_failure() {
        let mut manifest = make_test_manifest();
        let node = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
        node.columns.insert(
            "nonexistent_col".to_string(),
            ManifestColumn {
                name: "nonexistent_col".to_string(),
            },
        );
        let result = compute_column_lineage(
            &manifest,
            "stg_orders",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );
        assert_eq!(result.total_columns, 5);
        assert_eq!(result.traced_columns, 4);
    }

    #[test]
    fn test_traced_total_columns_model_not_found() {
        let manifest = make_test_manifest();
        let result = compute_column_lineage(
            &manifest,
            "nonexistent",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );
        assert_eq!(result.total_columns, 0);
        assert_eq!(result.traced_columns, 0);
    }

    #[test]
    fn test_traced_total_columns_in_json() {
        let manifest = make_test_manifest();
        let result = compute_column_lineage(
            &manifest,
            "stg_orders",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );
        let json = serde_json::to_string(&result).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["traced_columns"], 4);
        assert_eq!(parsed["total_columns"], 4);
    }

    // --- Regression tests for known issues ---

    #[test]
    fn test_cte_alias_resolution() {
        // Issue mml.6: FROM cte_name AS alias causes lineage to stop at alias
        // Pattern: WITH import_model AS (...) SELECT base.col FROM import_model AS base
        let mut nodes = HashMap::new();
        let mut sources = HashMap::new();

        // Source table
        let mut src_cols = HashMap::new();
        for name in ["id", "name", "status"] {
            src_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        sources.insert(
            "source.proj.raw.items".to_string(),
            ManifestSource {
                unique_id: "source.proj.raw.items".to_string(),
                name: "items".to_string(),
                source_name: "raw".to_string(),
                resource_type: "source".to_string(),
                description: None,
                path: None,
                columns: src_cols,
                database: None,
                schema: None,
                identifier: None,
            },
        );

        // stg_items: simple staging model
        let mut stg_cols = HashMap::new();
        for name in ["item_id", "name", "status"] {
            stg_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        nodes.insert(
            "model.proj.stg_items".to_string(),
            ManifestNode {
                unique_id: "model.proj.stg_items".to_string(),
                name: "stg_items".to_string(),
                resource_type: "model".to_string(),
                depends_on: DependsOn {
                    nodes: vec!["source.proj.raw.items".to_string()],
                },
                config: ManifestConfig::default(),
                description: None,
                path: None,
                columns: stg_cols,
                compiled_code: Some("select id as item_id, name, status from items".to_string()),
                database: None,
                schema: None,
            },
        );

        // mart_items: uses FROM cte AS alias pattern
        let mut mart_cols = HashMap::new();
        for name in ["item_id", "status"] {
            mart_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        nodes.insert(
            "model.proj.mart_items".to_string(),
            ManifestNode {
                unique_id: "model.proj.mart_items".to_string(),
                name: "mart_items".to_string(),
                resource_type: "model".to_string(),
                depends_on: DependsOn {
                    nodes: vec!["model.proj.stg_items".to_string()],
                },
                config: ManifestConfig::default(),
                description: None,
                path: None,
                columns: mart_cols,
                compiled_code: Some(
                    concat!(
                        "with import_stg_items as (\n",
                        "    select * from stg_items\n",
                        ")\n",
                        "select base.item_id, base.status\n",
                        "from import_stg_items as base"
                    )
                    .to_string(),
                ),
                database: None,
                schema: None,
            },
        );

        let manifest = Manifest {
            nodes,
            sources,
            exposures: HashMap::new(),
        };
        let result = compute_cross_model_column_lineage(
            &manifest,
            "mart_items",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.columns.len(), 2);

        // item_id should trace through stg_items to raw items.id
        // NOT stop at alias "base"
        let item_id = result
            .columns
            .iter()
            .find(|c| c.column == "item_id")
            .unwrap();
        assert!(
            item_id.sources.iter().all(|s| s.table != "base"),
            "item_id should not reference alias 'base', got: {:?}",
            item_id.sources
        );
        assert!(
            item_id.sources.iter().any(|s| s.column == "id"),
            "item_id should trace to raw items.id, got: {:?}",
            item_id.sources
        );
    }

    #[test]
    fn test_select_star_chain_with_join() {
        // Issue mml.7: SELECT * chain + JOIN causes "Cannot find column" errors
        let mut nodes = HashMap::new();
        let mut sources = HashMap::new();

        // Source: raw.users
        let mut user_cols = HashMap::new();
        for name in ["id", "name", "area"] {
            user_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        sources.insert(
            "source.proj.raw.users".to_string(),
            ManifestSource {
                unique_id: "source.proj.raw.users".to_string(),
                name: "users".to_string(),
                source_name: "raw".to_string(),
                resource_type: "source".to_string(),
                description: None,
                path: None,
                columns: user_cols,
                database: None,
                schema: None,
                identifier: None,
            },
        );

        // Source: raw.regions
        let mut region_cols = HashMap::new();
        for name in ["id", "region_name"] {
            region_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        sources.insert(
            "source.proj.raw.regions".to_string(),
            ManifestSource {
                unique_id: "source.proj.raw.regions".to_string(),
                name: "regions".to_string(),
                source_name: "raw".to_string(),
                resource_type: "source".to_string(),
                description: None,
                path: None,
                columns: region_cols,
                database: None,
                schema: None,
                identifier: None,
            },
        );

        // stg_users: SELECT * from raw
        let mut stg_user_cols = HashMap::new();
        for name in ["id", "name", "area"] {
            stg_user_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        nodes.insert(
            "model.proj.stg_users".to_string(),
            ManifestNode {
                unique_id: "model.proj.stg_users".to_string(),
                name: "stg_users".to_string(),
                resource_type: "model".to_string(),
                depends_on: DependsOn {
                    nodes: vec!["source.proj.raw.users".to_string()],
                },
                config: ManifestConfig::default(),
                description: None,
                path: None,
                columns: stg_user_cols,
                compiled_code: Some("select id, name, area from users".to_string()),
                database: Some("mydb".to_string()),
                schema: Some("myschema".to_string()),
            },
        );

        // stg_regions
        let mut stg_region_cols = HashMap::new();
        for name in ["id", "region_name"] {
            stg_region_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        nodes.insert(
            "model.proj.stg_regions".to_string(),
            ManifestNode {
                unique_id: "model.proj.stg_regions".to_string(),
                name: "stg_regions".to_string(),
                resource_type: "model".to_string(),
                depends_on: DependsOn {
                    nodes: vec!["source.proj.raw.regions".to_string()],
                },
                config: ManifestConfig::default(),
                description: None,
                path: None,
                columns: stg_region_cols,
                compiled_code: Some("select id, region_name from regions".to_string()),
                database: Some("mydb".to_string()),
                schema: Some("myschema".to_string()),
            },
        );

        // mart_users: multi-level SELECT * chain + JOIN
        // Uses backtick-quoted 3-part names like real dbt BigQuery compiled SQL
        let mut mart_cols = HashMap::new();
        for name in ["id", "name", "area", "region_name"] {
            mart_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        nodes.insert(
            "model.proj.mart_users".to_string(),
            ManifestNode {
                unique_id: "model.proj.mart_users".to_string(),
                name: "mart_users".to_string(),
                resource_type: "model".to_string(),
                depends_on: DependsOn {
                    nodes: vec![
                        "model.proj.stg_users".to_string(),
                        "model.proj.stg_regions".to_string(),
                    ],
                },
                config: ManifestConfig::default(),
                description: None,
                path: None,
                columns: mart_cols,
                compiled_code: Some(
                    concat!(
                        "with\n",
                        "import_users as (\n",
                        "    select * from `mydb`.`myschema`.`stg_users`\n",
                        "),\n",
                        "base as (\n",
                        "    select * from import_users\n",
                        "),\n",
                        "import_regions as (\n",
                        "    select * from `mydb`.`myschema`.`stg_regions`\n",
                        ")\n",
                        "select base.*, import_regions.region_name\n",
                        "from base\n",
                        "left join import_regions on base.area = import_regions.id"
                    )
                    .to_string(),
                ),
                database: Some("mydb".to_string()),
                schema: Some("myschema".to_string()),
            },
        );

        let manifest = Manifest {
            nodes,
            sources,
            exposures: HashMap::new(),
        };
        let result = compute_cross_model_column_lineage(
            &manifest,
            "mart_users",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        // All 4 columns should resolve without errors
        assert!(
            result.errors.is_empty(),
            "should resolve all columns without errors, got: {:?}",
            result.errors
        );
        assert_eq!(
            result.columns.len(),
            4,
            "should have 4 columns, got: {:?}",
            result.columns.iter().map(|c| &c.column).collect::<Vec<_>>()
        );

        // area should trace through to raw users source
        let area = result.columns.iter().find(|c| c.column == "area").unwrap();
        assert!(
            area.sources
                .iter()
                .any(|s| s.column == "area" && s.table.contains("users")),
            "area should trace to raw users.area, got: {:?}",
            area.sources
        );

        // region_name should trace through to raw regions source
        let region = result
            .columns
            .iter()
            .find(|c| c.column == "region_name")
            .unwrap();
        assert!(
            region
                .sources
                .iter()
                .any(|s| s.column == "region_name" && s.table.contains("regions")),
            "region_name should trace to raw regions.region_name, got: {:?}",
            region.sources
        );
    }

    #[test]
    fn test_select_star_chain_with_cte_alias_and_join() {
        // Combination of mml.6 + mml.7: SELECT * chain + CTE alias + JOIN
        // This is the most common dbt pattern in mart/warehouse layers
        let mut nodes = HashMap::new();
        let mut sources = HashMap::new();

        // Source: raw.users
        let mut user_cols = HashMap::new();
        for name in ["id", "name", "area"] {
            user_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        sources.insert(
            "source.proj.raw.users".to_string(),
            ManifestSource {
                unique_id: "source.proj.raw.users".to_string(),
                name: "users".to_string(),
                source_name: "raw".to_string(),
                resource_type: "source".to_string(),
                description: None,
                path: None,
                columns: user_cols,
                database: None,
                schema: None,
                identifier: None,
            },
        );

        // Source: raw.regions
        let mut region_cols = HashMap::new();
        for name in ["id", "region_name"] {
            region_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        sources.insert(
            "source.proj.raw.regions".to_string(),
            ManifestSource {
                unique_id: "source.proj.raw.regions".to_string(),
                name: "regions".to_string(),
                source_name: "raw".to_string(),
                resource_type: "source".to_string(),
                description: None,
                path: None,
                columns: region_cols,
                database: None,
                schema: None,
                identifier: None,
            },
        );

        // stg_users
        let mut stg_user_cols = HashMap::new();
        for name in ["id", "name", "area"] {
            stg_user_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        nodes.insert(
            "model.proj.stg_users".to_string(),
            ManifestNode {
                unique_id: "model.proj.stg_users".to_string(),
                name: "stg_users".to_string(),
                resource_type: "model".to_string(),
                depends_on: DependsOn {
                    nodes: vec!["source.proj.raw.users".to_string()],
                },
                config: ManifestConfig::default(),
                description: None,
                path: None,
                columns: stg_user_cols,
                compiled_code: Some("select id, name, area from users".to_string()),
                database: Some("mydb".to_string()),
                schema: Some("myschema".to_string()),
            },
        );

        // stg_regions
        let mut stg_region_cols = HashMap::new();
        for name in ["id", "region_name"] {
            stg_region_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        nodes.insert(
            "model.proj.stg_regions".to_string(),
            ManifestNode {
                unique_id: "model.proj.stg_regions".to_string(),
                name: "stg_regions".to_string(),
                resource_type: "model".to_string(),
                depends_on: DependsOn {
                    nodes: vec!["source.proj.raw.regions".to_string()],
                },
                config: ManifestConfig::default(),
                description: None,
                path: None,
                columns: stg_region_cols,
                compiled_code: Some("select id, region_name from regions".to_string()),
                database: Some("mydb".to_string()),
                schema: Some("myschema".to_string()),
            },
        );

        // mart_users: SELECT * chain + CTE alias + JOIN
        // Pattern from mml.7 description but with CTE aliases (mml.6)
        let mut mart_cols = HashMap::new();
        for name in ["id", "name", "area", "region_name"] {
            mart_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        nodes.insert(
            "model.proj.mart_users".to_string(),
            ManifestNode {
                unique_id: "model.proj.mart_users".to_string(),
                name: "mart_users".to_string(),
                resource_type: "model".to_string(),
                depends_on: DependsOn {
                    nodes: vec![
                        "model.proj.stg_users".to_string(),
                        "model.proj.stg_regions".to_string(),
                    ],
                },
                config: ManifestConfig::default(),
                description: None,
                path: None,
                columns: mart_cols,
                compiled_code: Some(
                    concat!(
                        "with\n",
                        "import_users as (\n",
                        "    select * from `mydb`.`myschema`.`stg_users`\n",
                        "),\n",
                        "import_regions as (\n",
                        "    select * from `mydb`.`myschema`.`stg_regions`\n",
                        ")\n",
                        "select u.*, import_regions.region_name\n",
                        "from import_users as u\n",
                        "left join import_regions on u.area = import_regions.id"
                    )
                    .to_string(),
                ),
                database: Some("mydb".to_string()),
                schema: Some("myschema".to_string()),
            },
        );

        let manifest = Manifest {
            nodes,
            sources,
            exposures: HashMap::new(),
        };
        let result = compute_cross_model_column_lineage(
            &manifest,
            "mart_users",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        // All 4 columns should resolve without errors
        assert!(
            result.errors.is_empty(),
            "should resolve all columns without errors, got: {:?}",
            result.errors
        );
        assert_eq!(
            result.columns.len(),
            4,
            "should have 4 columns, got: {:?}",
            result.columns.iter().map(|c| &c.column).collect::<Vec<_>>()
        );

        // area should trace through CTE alias "u" → import_users → stg_users → raw users
        let area = result.columns.iter().find(|c| c.column == "area").unwrap();
        assert!(
            area.sources
                .iter()
                .any(|s| s.column == "area" && s.table.contains("users")),
            "area should trace to raw users.area, got: {:?}",
            area.sources
        );

        // region_name should trace through import_regions → stg_regions → raw regions
        let region = result
            .columns
            .iter()
            .find(|c| c.column == "region_name")
            .unwrap();
        assert!(
            region
                .sources
                .iter()
                .any(|s| s.column == "region_name" && s.table.contains("regions")),
            "region_name should trace to raw regions.region_name, got: {:?}",
            region.sources
        );
    }

    // --- Column impact tests ---

    #[test]
    fn test_column_impact_direct_dependent() {
        // stg_orders.order_id is used by orders.order_id
        let manifest = make_cross_model_manifest();
        let result = compute_column_impact(
            &manifest,
            "stg_orders",
            "order_id",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert!(
            result
                .impacted_columns
                .iter()
                .any(|ic| ic.model == "orders" && ic.column == "order_id"),
            "orders.order_id should be impacted, got: {:?}",
            result.impacted_columns
        );
    }

    #[test]
    fn test_column_impact_two_hops() {
        // stg_orders.order_id → orders.order_id → customers (via count)
        // stg_orders.customer_id → orders.customer_id → customers.customer_id
        let manifest = make_cross_model_manifest();
        let result = compute_column_impact(
            &manifest,
            "stg_orders",
            "customer_id",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        // orders.customer_id should be impacted (direct dependent)
        assert!(
            result
                .impacted_columns
                .iter()
                .any(|ic| ic.model == "orders" && ic.column == "customer_id"),
            "orders.customer_id should be impacted, got: {:?}",
            result.impacted_columns
        );
        // customers.customer_id should also be impacted (two hops)
        assert!(
            result
                .impacted_columns
                .iter()
                .any(|ic| ic.model == "customers" && ic.column == "customer_id"),
            "customers.customer_id should be impacted, got: {:?}",
            result.impacted_columns
        );
    }

    #[test]
    fn test_column_impact_model_path() {
        let manifest = make_cross_model_manifest();
        let result = compute_column_impact(
            &manifest,
            "stg_orders",
            "customer_id",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        // customers.customer_id goes through orders
        let cust = result
            .impacted_columns
            .iter()
            .find(|ic| ic.model == "customers" && ic.column == "customer_id")
            .unwrap();
        assert!(
            cust.model_path.contains(&"orders".to_string()),
            "model_path should include orders, got: {:?}",
            cust.model_path
        );
    }

    #[test]
    fn test_column_impact_no_dependents() {
        // customers is a leaf model — no downstream
        let manifest = make_cross_model_manifest();
        let result = compute_column_impact(
            &manifest,
            "customers",
            "customer_id",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert!(
            result.impacted_columns.is_empty(),
            "leaf model should have no impacted columns, got: {:?}",
            result.impacted_columns
        );
    }

    #[test]
    fn test_column_impact_model_not_found() {
        let manifest = make_cross_model_manifest();
        let result = compute_column_impact(
            &manifest,
            "nonexistent",
            "col",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        assert!(!result.errors.is_empty());
        assert!(result.errors[0].contains("not found"));
    }

    #[test]
    fn test_column_impact_json_serialization() {
        let manifest = make_cross_model_manifest();
        let result = compute_column_impact(
            &manifest,
            "stg_orders",
            "order_id",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );
        let json = serde_json::to_string_pretty(&result).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["model"], "stg_orders");
        assert_eq!(parsed["column"], "order_id");
        assert!(parsed["impacted_columns"].is_array());
    }

    /// Build a diamond DAG manifest where different columns flow through a shared model:
    ///
    ///   raw_data (x, y)
    ///      |
    ///   shared (x, y)   -- passes both columns through
    ///    /        \
    ///  left(x)  right(y)  -- each uses a different column from shared
    ///    \        /
    ///   diamond_out(lx, ry) -- combines left.x and right.y
    fn make_diamond_manifest() -> Manifest {
        let mut nodes = HashMap::new();

        // raw_data: source with columns x, y
        let mut raw_cols = HashMap::new();
        for name in ["x", "y"] {
            raw_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        nodes.insert(
            "model.proj.raw_data".to_string(),
            ManifestNode {
                unique_id: "model.proj.raw_data".to_string(),
                name: "raw_data".to_string(),
                resource_type: "model".to_string(),
                depends_on: DependsOn { nodes: vec![] },
                config: ManifestConfig::default(),
                description: None,
                path: None,
                columns: raw_cols,
                compiled_code: Some("select x, y from source_table".to_string()),
                database: None,
                schema: None,
            },
        );

        // shared: passes x and y through from raw_data
        let mut shared_cols = HashMap::new();
        for name in ["x", "y"] {
            shared_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        nodes.insert(
            "model.proj.shared".to_string(),
            ManifestNode {
                unique_id: "model.proj.shared".to_string(),
                name: "shared".to_string(),
                resource_type: "model".to_string(),
                depends_on: DependsOn {
                    nodes: vec!["model.proj.raw_data".to_string()],
                },
                config: ManifestConfig::default(),
                description: None,
                path: None,
                columns: shared_cols,
                compiled_code: Some("select x, y from raw_data".to_string()),
                database: None,
                schema: None,
            },
        );

        // left: uses column x from shared
        let mut left_cols = HashMap::new();
        left_cols.insert(
            "x".to_string(),
            ManifestColumn {
                name: "x".to_string(),
            },
        );
        nodes.insert(
            "model.proj.left_model".to_string(),
            ManifestNode {
                unique_id: "model.proj.left_model".to_string(),
                name: "left_model".to_string(),
                resource_type: "model".to_string(),
                depends_on: DependsOn {
                    nodes: vec!["model.proj.shared".to_string()],
                },
                config: ManifestConfig::default(),
                description: None,
                path: None,
                columns: left_cols,
                compiled_code: Some("select x from shared".to_string()),
                database: None,
                schema: None,
            },
        );

        // right: uses column y from shared
        let mut right_cols = HashMap::new();
        right_cols.insert(
            "y".to_string(),
            ManifestColumn {
                name: "y".to_string(),
            },
        );
        nodes.insert(
            "model.proj.right_model".to_string(),
            ManifestNode {
                unique_id: "model.proj.right_model".to_string(),
                name: "right_model".to_string(),
                resource_type: "model".to_string(),
                depends_on: DependsOn {
                    nodes: vec!["model.proj.shared".to_string()],
                },
                config: ManifestConfig::default(),
                description: None,
                path: None,
                columns: right_cols,
                compiled_code: Some("select y from shared".to_string()),
                database: None,
                schema: None,
            },
        );

        // diamond_out: combines left.x and right.y
        let mut out_cols = HashMap::new();
        for name in ["lx", "ry"] {
            out_cols.insert(
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            );
        }
        nodes.insert(
            "model.proj.diamond_out".to_string(),
            ManifestNode {
                unique_id: "model.proj.diamond_out".to_string(),
                name: "diamond_out".to_string(),
                resource_type: "model".to_string(),
                depends_on: DependsOn {
                    nodes: vec![
                        "model.proj.left_model".to_string(),
                        "model.proj.right_model".to_string(),
                    ],
                },
                config: ManifestConfig::default(),
                description: None,
                path: None,
                columns: out_cols,
                compiled_code: Some(
                    "select l.x as lx, r.y as ry from left_model l join right_model r on 1=1"
                        .to_string(),
                ),
                database: None,
                schema: None,
            },
        );

        Manifest {
            nodes,
            sources: HashMap::new(),
            exposures: HashMap::new(),
        }
    }

    #[test]
    fn test_cross_model_diamond_different_columns_through_shared_model() {
        // In a diamond DAG, different columns (x and y) flow through a shared
        // upstream model. Both should be resolved independently — the visited set
        // must not truncate the second column's path through the shared model.
        let manifest = make_diamond_manifest();

        // Verify left_model traces x through shared to raw_data
        let left = compute_cross_model_column_lineage(
            &manifest,
            "left_model",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );
        assert!(left.errors.is_empty(), "left errors: {:?}", left.errors);
        let left_x = left.columns.iter().find(|c| c.column == "x").unwrap();
        assert!(
            left_x.sources.iter().any(|s| s.column == "x"),
            "left_model.x should trace through shared, got: {:?}",
            left_x.sources
        );

        // Verify right_model traces y through shared to raw_data
        let right = compute_cross_model_column_lineage(
            &manifest,
            "right_model",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );
        assert!(right.errors.is_empty(), "right errors: {:?}", right.errors);
        let right_y = right.columns.iter().find(|c| c.column == "y").unwrap();
        assert!(
            right_y.sources.iter().any(|s| s.column == "y"),
            "right_model.y should trace through shared, got: {:?}",
            right_y.sources
        );

        // Both left and right depend on 'shared' — the key assertion is that
        // resolving one does not prevent the other from being resolved.
        // With the old model-only visited set, whichever resolved first would
        // block the other from tracing through 'shared'.
        assert!(
            !left_x.sources.is_empty() && !right_y.sources.is_empty(),
            "both paths through shared should resolve independently"
        );
    }

    #[test]
    fn test_column_impact_diamond_different_columns_through_shared_model() {
        // Impact of raw_data.x should flow through shared → left_model
        // Impact of raw_data.y should flow through shared → right_model
        // Both should be detected independently despite sharing the 'shared' model.
        let manifest = make_diamond_manifest();

        let impact_x = compute_column_impact(
            &manifest,
            "raw_data",
            "x",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );
        assert!(impact_x.errors.is_empty(), "errors: {:?}", impact_x.errors);

        let impacted_names: Vec<(&str, &str)> = impact_x
            .impacted_columns
            .iter()
            .map(|ic| (ic.model.as_str(), ic.column.as_str()))
            .collect();
        assert!(
            impacted_names.contains(&("shared", "x")),
            "x should impact shared.x, got: {:?}",
            impacted_names
        );
        assert!(
            impacted_names.contains(&("left_model", "x")),
            "x should impact left_model.x, got: {:?}",
            impacted_names
        );
        // x should NOT impact right_model.y
        assert!(
            !impacted_names.contains(&("right_model", "y")),
            "x should not impact right_model.y"
        );

        let impact_y = compute_column_impact(
            &manifest,
            "raw_data",
            "y",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );
        assert!(impact_y.errors.is_empty(), "errors: {:?}", impact_y.errors);

        let impacted_names_y: Vec<(&str, &str)> = impact_y
            .impacted_columns
            .iter()
            .map(|ic| (ic.model.as_str(), ic.column.as_str()))
            .collect();
        assert!(
            impacted_names_y.contains(&("shared", "y")),
            "y should impact shared.y, got: {:?}",
            impacted_names_y
        );
        assert!(
            impacted_names_y.contains(&("right_model", "y")),
            "y should impact right_model.y, got: {:?}",
            impacted_names_y
        );
        // y should NOT impact left_model.x
        assert!(
            !impacted_names_y.contains(&("left_model", "x")),
            "y should not impact left_model.x"
        );
    }

    #[test]
    fn test_build_downstream_model_map() {
        let manifest = make_cross_model_manifest();
        let map = build_downstream_model_map(&manifest);

        // stg_orders is depended on by orders
        assert!(
            map.get("stg_orders")
                .map_or(false, |deps| deps.contains(&"orders".to_string())),
            "stg_orders should have orders as downstream, got: {:?}",
            map.get("stg_orders")
        );
        // orders is depended on by customers
        assert!(
            map.get("orders")
                .map_or(false, |deps| deps.contains(&"customers".to_string())),
            "orders should have customers as downstream, got: {:?}",
            map.get("orders")
        );
        // customers has no downstream
        assert!(
            map.get("customers").is_none(),
            "customers should have no downstream"
        );
    }

    // --- ColumnLineageCache tests ---

    #[test]
    fn test_column_cache_hit() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path();

        let mut cache = ColumnLineageCache::load(project_dir, None);
        let lineage = ModelColumnLineage {
            model: "test_model".to_string(),
            traced_columns: 1,
            total_columns: 1,
            columns: vec![ColumnLineageEntry {
                column: "id".to_string(),
                transformation: TransformationType::Direct,
                sources: vec![ColumnSource {
                    table: "raw".to_string(),
                    column: "id".to_string(),
                    model_path: vec![],
                }],
            }],
            errors: vec![],
        };
        cache.insert(
            "test_model",
            "SELECT id FROM raw",
            DialectType::Generic,
            0,
            lineage,
        );
        cache.save();

        // Reload from disk
        let cache2 = ColumnLineageCache::load(project_dir, None);
        let hit = cache2
            .get("test_model", "SELECT id FROM raw", DialectType::Generic, 0)
            .unwrap();
        assert_eq!(hit.columns.len(), 1);
        assert_eq!(hit.columns[0].column, "id");
    }

    #[test]
    fn test_column_cache_miss_on_code_change() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path();

        let mut cache = ColumnLineageCache::load(project_dir, None);
        let lineage = ModelColumnLineage {
            model: "m".to_string(),
            traced_columns: 0,
            total_columns: 0,
            columns: vec![],
            errors: vec![],
        };
        cache.insert("m", "SELECT 1", DialectType::Generic, 0, lineage);
        cache.save();

        let cache2 = ColumnLineageCache::load(project_dir, None);
        assert!(
            cache2
                .get("m", "SELECT 2", DialectType::Generic, 0)
                .is_none()
        );
    }

    #[test]
    fn test_column_cache_miss_on_dialect_change() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path();

        let mut cache = ColumnLineageCache::load(project_dir, None);
        let lineage = ModelColumnLineage {
            model: "m".to_string(),
            traced_columns: 0,
            total_columns: 0,
            columns: vec![],
            errors: vec![],
        };
        cache.insert("m", "SELECT 1", DialectType::BigQuery, 0, lineage);
        cache.save();

        let cache2 = ColumnLineageCache::load(project_dir, None);
        assert!(
            cache2
                .get("m", "SELECT 1", DialectType::Snowflake, 0)
                .is_none()
        );
    }

    #[test]
    fn test_column_cache_miss_on_manifest_columns_change() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path();

        let mut cache = ColumnLineageCache::load(project_dir, None);
        let lineage = ModelColumnLineage {
            model: "m".to_string(),
            traced_columns: 0,
            total_columns: 0,
            columns: vec![],
            errors: vec![],
        };
        cache.insert("m", "SELECT 1", DialectType::Generic, 42, lineage);
        cache.save();

        let cache2 = ColumnLineageCache::load(project_dir, None);
        // Same hash → hit
        assert!(
            cache2
                .get("m", "SELECT 1", DialectType::Generic, 42)
                .is_some()
        );
        // Different hash → miss (YAML columns changed in manifest)
        assert!(
            cache2
                .get("m", "SELECT 1", DialectType::Generic, 99)
                .is_none()
        );
    }

    #[test]
    fn test_column_cache_version_invalidation() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path();

        let mut cache = ColumnLineageCache::load(project_dir, None);
        let lineage = ModelColumnLineage {
            model: "m".to_string(),
            traced_columns: 0,
            total_columns: 0,
            columns: vec![],
            errors: vec![],
        };
        cache.insert("m", "SELECT 1", DialectType::Generic, 0, lineage);
        cache.save();

        // Tamper with version in saved file
        let cache_path = project_dir
            .join(CACHE_DIR)
            .join(COLUMN_LINEAGE_CACHE_FILENAME);
        let content = std::fs::read_to_string(&cache_path).unwrap();
        let mut cf: ColumnLineageCacheFile = serde_json::from_str(&content).unwrap();
        cf.version = "0.0.0-fake".to_string();
        std::fs::write(&cache_path, serde_json::to_string(&cf).unwrap()).unwrap();

        let cache2 = ColumnLineageCache::load(project_dir, None);
        assert!(
            cache2
                .get("m", "SELECT 1", DialectType::Generic, 0)
                .is_none()
        );
    }

    #[test]
    fn test_column_cache_disabled() {
        let mut cache = ColumnLineageCache::disabled();
        let lineage = ModelColumnLineage {
            model: "m".to_string(),
            traced_columns: 0,
            total_columns: 0,
            columns: vec![],
            errors: vec![],
        };
        cache.insert("m", "SELECT 1", DialectType::Generic, 0, lineage);
        // Disabled cache still works in-memory (only disk persistence is disabled)
        assert!(
            cache
                .get("m", "SELECT 1", DialectType::Generic, 0)
                .is_some()
        );
        // But save is a no-op (no cache_path)
        cache.save();
    }

    #[test]
    fn test_column_cache_fresh() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path();

        // Populate cache
        let mut cache = ColumnLineageCache::load(project_dir, None);
        let lineage = ModelColumnLineage {
            model: "m".to_string(),
            traced_columns: 0,
            total_columns: 0,
            columns: vec![],
            errors: vec![],
        };
        cache.insert("m", "SELECT 1", DialectType::Generic, 0, lineage);
        cache.save();

        // Fresh cache ignores existing entries
        let fresh = ColumnLineageCache::fresh(project_dir, None);
        assert!(
            fresh
                .get("m", "SELECT 1", DialectType::Generic, 0)
                .is_none()
        );

        // But can save new entries
        let mut fresh = ColumnLineageCache::fresh(project_dir, None);
        let lineage2 = ModelColumnLineage {
            model: "m2".to_string(),
            traced_columns: 0,
            total_columns: 0,
            columns: vec![],
            errors: vec![],
        };
        fresh.insert("m2", "SELECT 2", DialectType::Generic, 0, lineage2);
        fresh.save();

        let reloaded = ColumnLineageCache::load(project_dir, None);
        assert!(
            reloaded
                .get("m2", "SELECT 2", DialectType::Generic, 0)
                .is_some()
        );
    }
}
