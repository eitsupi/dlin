use std::collections::{HashMap, HashSet, VecDeque};

use minijinja::Environment;

use super::source::{
    ModelMacroSpan, model_macro_definition_spans, strip_macro_definitions_for_runtime_analysis,
};

mod analysis;
mod catalog;
#[cfg(test)]
use analysis::local_macro_dependencies_with_status;
use analysis::{free_local_symbols, macro_dependencies_with_status, valid_span};
use catalog::PrefixCatalog;
pub(crate) use catalog::PreparedMacroPrefix;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MacroReachability {
    pub(crate) local_scopes: HashSet<String>,
    /// Source spans for every reachable local definition. Duplicate macro
    /// names intentionally retain all definitions for conservative recovery.
    pub(crate) local_definition_spans: Vec<ModelMacroSpan>,
    pub(crate) prefix_scopes: HashSet<String>,
    /// Source spans for every reachable prefix definition. Duplicate macro
    /// names intentionally retain all definitions for conservative recovery.
    pub(crate) prefix_definition_spans: Vec<ModelMacroSpan>,
    pub(crate) local_macro_count: usize,
    pub(crate) prefix_macro_count: usize,
    pub(crate) prefix_uses_runtime_scalar: bool,
}

#[cfg(test)]
mod tests;

/// Compute the local macro definitions that can be reached from the model
/// source and reachable project macros using MiniJinja's free-symbol analysis.
///
/// This intentionally does not parse call syntax. MiniJinja already accounts
/// for aliases, higher-order values, collections, conditional assignments,
/// and lexical shadowing when it computes undeclared variables. A failed
/// compile means that the provenance is unknown, so callers must use the
/// conservative whole-model fallback instead. `macro_prefix` is analyzed only
/// for macro names that the model can reach, so unrelated project macros do
/// not incur a MiniJinja compilation.
#[cfg(test)]
fn reachable_local_macros_with_prefix(
    sql: &str,
    macro_prefix: &str,
    known_spans: Option<&[ModelMacroSpan]>,
    known_roots: Option<&HashSet<String>>,
) -> Option<(HashSet<String>, usize)> {
    macro_reachability_with_prefix(sql, macro_prefix, known_spans, known_roots)
        .map(|plan| (plan.local_scopes, plan.local_macro_count))
}

#[cfg(test)]
fn reachable_local_macros_with_status_and_prefix(
    sql: &str,
    macro_prefix: &str,
    known_spans: Option<&[ModelMacroSpan]>,
    known_roots: Option<&HashSet<String>>,
) -> Option<(HashSet<String>, usize, bool, usize)> {
    macro_reachability_with_prefix_with_status(sql, macro_prefix, known_spans, known_roots).map(
        |(plan, root_compile_performed, definition_compile_count)| {
            (
                plan.local_scopes,
                plan.local_macro_count,
                root_compile_performed,
                definition_compile_count,
            )
        },
    )
}

#[cfg(test)]
pub(crate) fn macro_reachability_with_prefix(
    sql: &str,
    macro_prefix: &str,
    known_spans: Option<&[ModelMacroSpan]>,
    known_roots: Option<&HashSet<String>>,
) -> Option<MacroReachability> {
    macro_reachability_with_prefix_and_spans(sql, macro_prefix, known_spans, None, known_roots)
}

/// Variant of [`macro_reachability_with_prefix`] that reuses prefix spans
/// discovered by runtime analysis. `None` keeps the standalone behavior and
/// scans the prefix source here.
#[cfg(test)]
pub(crate) fn macro_reachability_with_prefix_and_spans(
    sql: &str,
    macro_prefix: &str,
    known_spans: Option<&[ModelMacroSpan]>,
    known_prefix_spans: Option<&[ModelMacroSpan]>,
    known_roots: Option<&HashSet<String>>,
) -> Option<MacroReachability> {
    macro_reachability_with_prefix_with_status_and_spans(
        sql,
        macro_prefix,
        known_spans,
        known_prefix_spans,
        known_roots,
    )
    .map(|(plan, _, _)| plan)
}

/// Compute reachability using an immutable project-level prefix catalog. The
/// catalog's MiniJinja analysis is shared across all models in a build.
pub(crate) fn macro_reachability_with_prepared_prefix(
    sql: &str,
    macro_prefix: &PreparedMacroPrefix,
    known_spans: Option<&[ModelMacroSpan]>,
    known_roots: Option<&HashSet<String>>,
) -> Option<MacroReachability> {
    let catalog = macro_prefix.catalog()?;
    macro_reachability_with_status_and_catalog(
        sql,
        macro_prefix.source(),
        known_spans,
        Some(macro_prefix.spans()),
        known_roots,
        Some(catalog),
    )
    .map(|(plan, _, _)| plan)
}

#[cfg(test)]
fn macro_reachability_with_prefix_with_status(
    sql: &str,
    macro_prefix: &str,
    known_spans: Option<&[ModelMacroSpan]>,
    known_roots: Option<&HashSet<String>>,
) -> Option<(MacroReachability, bool, usize)> {
    macro_reachability_with_prefix_with_status_and_spans(
        sql,
        macro_prefix,
        known_spans,
        None,
        known_roots,
    )
}

#[cfg(test)]
fn macro_reachability_with_prefix_with_status_and_spans(
    sql: &str,
    macro_prefix: &str,
    known_spans: Option<&[ModelMacroSpan]>,
    known_prefix_spans: Option<&[ModelMacroSpan]>,
    known_roots: Option<&HashSet<String>>,
) -> Option<(MacroReachability, bool, usize)> {
    macro_reachability_with_status_and_catalog(
        sql,
        macro_prefix,
        known_spans,
        known_prefix_spans,
        known_roots,
        None,
    )
}

fn macro_reachability_with_status_and_catalog(
    sql: &str,
    macro_prefix: &str,
    known_spans: Option<&[ModelMacroSpan]>,
    known_prefix_spans: Option<&[ModelMacroSpan]>,
    known_roots: Option<&HashSet<String>>,
    prefix_catalog: Option<&PrefixCatalog>,
) -> Option<(MacroReachability, bool, usize)> {
    let spans = known_spans.map_or_else(
        || model_macro_definition_spans(sql),
        <[ModelMacroSpan]>::to_vec,
    );
    let prefix_spans = known_prefix_spans.map_or_else(
        || model_macro_definition_spans(macro_prefix),
        <[ModelMacroSpan]>::to_vec,
    );
    if spans.iter().any(|span| !valid_span(sql, span))
        || prefix_spans
            .iter()
            .any(|span| !valid_span(macro_prefix, span))
    {
        return None;
    }
    let local_names: HashSet<String> = spans.iter().map(|span| span.name.clone()).collect();
    let local_macro_count = local_names.len();
    let prefix_names: HashSet<String> = prefix_spans.iter().map(|span| span.name.clone()).collect();
    let all_names: HashSet<String> = local_names.union(&prefix_names).cloned().collect();
    let env = Environment::new();

    let (roots, root_compile_performed) = if let Some(roots) = known_roots {
        (
            roots
                .intersection(&all_names)
                .cloned()
                .collect::<HashSet<_>>(),
            false,
        )
    } else {
        let model_source = strip_macro_definitions_for_runtime_analysis(sql, &spans);
        (free_local_symbols(&env, &model_source, &all_names)?, true)
    };
    let local_roots: HashSet<String> = roots.intersection(&local_names).cloned().collect();
    // Every local definition is already a root, so local expansion can be
    // skipped. Prefix roots still need expansion to recover prefix ownership
    // and scalar uncertainty.
    let local_roots_cover_all = local_macro_count > 0 && local_roots.len() == local_macro_count;
    let mut definitions_by_name = HashMap::<String, Vec<&str>>::new();
    for span in &spans {
        let definition = &sql[span.start..span.end];
        // When every local macro is already a root, only local definitions
        // that could introduce a new prefix edge need expansion. This keeps
        // the dense no-prefix case at zero local definition compiles while
        // preserving local -> prefix -> local scalar reachability.
        if !local_roots_cover_all || prefix_names.iter().any(|name| definition.contains(name)) {
            definitions_by_name
                .entry(span.name.clone())
                .or_default()
                .push(definition);
        }
    }
    let mut prefix_definitions_by_name = HashMap::<String, Vec<&str>>::new();
    if prefix_catalog.is_none() {
        for span in &prefix_spans {
            prefix_definitions_by_name
                .entry(span.name.clone())
                .or_default()
                .push(&macro_prefix[span.start..span.end]);
        }
    }
    let mut reachable = local_roots;
    let mut pending: VecDeque<String> = roots.into_iter().collect();
    let mut expanded = HashSet::new();
    let mut definition_compile_count = 0;
    let mut prefix_uses_runtime_scalar = false;
    while let Some(name) = pending.pop_front() {
        if !expanded.insert(name.clone()) {
            continue;
        }
        // Compile only definitions that are reachable from the model. If a
        // name is duplicated, conservatively union every definition under it.
        if let Some(definitions) = definitions_by_name.remove(&name) {
            for definition in definitions {
                let (dependencies, compiled, _uses_runtime_scalar) =
                    macro_dependencies_with_status(&env, definition, &name, &all_names)?;
                definition_compile_count += usize::from(compiled);
                for dependency in dependencies {
                    if local_names.contains(&dependency) {
                        reachable.insert(dependency.clone());
                    }
                    if all_names.contains(&dependency) {
                        pending.push_back(dependency);
                    }
                }
            }
        }
        if let Some(definitions) = prefix_definitions_by_name.remove(&name) {
            for definition in definitions {
                let (dependencies, compiled, uses_runtime_scalar) =
                    macro_dependencies_with_status(&env, definition, &name, &all_names)?;
                definition_compile_count += usize::from(compiled);
                prefix_uses_runtime_scalar |= uses_runtime_scalar;
                for dependency in dependencies {
                    if local_names.contains(&dependency) {
                        reachable.insert(dependency.clone());
                    }
                    if all_names.contains(&dependency) {
                        pending.push_back(dependency);
                    }
                }
            }
        }
        if let Some(prefix_catalog) = prefix_catalog
            && let Some(indices) = prefix_catalog.definitions_by_name.get(&name)
        {
            for index in indices {
                let definition = &prefix_catalog.definitions[*index];
                let analysis = definition.analysis(macro_prefix)?;
                let dependencies = analysis
                    .free_symbols
                    .intersection(&all_names)
                    .cloned()
                    .collect::<HashSet<_>>();
                definition_compile_count += 1;
                prefix_uses_runtime_scalar |= analysis.uses_runtime_scalar;
                for dependency in dependencies {
                    if local_names.contains(&dependency) {
                        reachable.insert(dependency.clone());
                    }
                    if all_names.contains(&dependency) {
                        pending.push_back(dependency);
                    }
                }
            }
        }
    }
    let local_definition_spans = spans
        .iter()
        .filter(|span| reachable.contains(&span.name))
        .cloned()
        .collect();
    let prefix_scopes: HashSet<String> = expanded.intersection(&prefix_names).cloned().collect();
    let prefix_definition_spans = prefix_spans
        .into_iter()
        .filter(|span| prefix_scopes.contains(&span.name))
        .collect();
    Some((
        MacroReachability {
            local_scopes: reachable,
            local_definition_spans,
            prefix_scopes,
            prefix_definition_spans,
            local_macro_count,
            prefix_macro_count: prefix_names.len(),
            prefix_uses_runtime_scalar,
        },
        root_compile_performed,
        definition_compile_count,
    ))
}
