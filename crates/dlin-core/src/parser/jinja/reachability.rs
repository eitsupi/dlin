use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{
    OnceLock,
    atomic::{AtomicUsize, Ordering},
};

use minijinja::Environment;

use super::source::{
    ModelMacroSpan, model_macro_definition_spans, strip_macro_definitions_for_runtime_analysis,
};

const RUNTIME_SCALAR_NAMES: [&str; 4] =
    ["execute", "dbt_version", "invocation_id", "run_started_at"];

/// Immutable project-level macro prefix state shared by model extraction.
/// Definition spans are collected eagerly, while MiniJinja free-symbol
/// analysis is initialized only when a runtime reachability query needs it.
#[derive(Debug)]
pub(crate) struct PreparedMacroPrefix {
    source: String,
    spans: Vec<ModelMacroSpan>,
    catalog: OnceLock<Option<PrefixCatalog>>,
    catalog_initializations: AtomicUsize,
}

#[derive(Debug)]
struct PrefixCatalog {
    definitions: Vec<PrefixMacroDefinition>,
    definitions_by_name: HashMap<String, Vec<usize>>,
}

#[derive(Debug)]
struct PrefixMacroDefinition {
    span: ModelMacroSpan,
    analysis: OnceLock<Option<PrefixMacroAnalysis>>,
}

#[derive(Debug)]
struct PrefixMacroAnalysis {
    free_symbols: HashSet<String>,
    uses_runtime_scalar: bool,
}

impl PreparedMacroPrefix {
    pub(crate) fn new(source: &str) -> Self {
        Self {
            source: source.to_owned(),
            spans: model_macro_definition_spans(source),
            catalog: OnceLock::new(),
            catalog_initializations: AtomicUsize::new(0),
        }
    }

    pub(crate) fn source(&self) -> &str {
        &self.source
    }

    pub(crate) fn spans(&self) -> &[ModelMacroSpan] {
        &self.spans
    }

    #[cfg(test)]
    pub(crate) fn catalog_initializations(&self) -> usize {
        self.catalog_initializations.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn initialized_definition_count(&self) -> usize {
        self.catalog
            .get()
            .and_then(Option::as_ref)
            .map_or(0, |catalog| {
                catalog
                    .definitions
                    .iter()
                    .filter(|definition| definition.analysis.get().is_some())
                    .count()
            })
    }

    fn catalog(&self) -> Option<&PrefixCatalog> {
        self.catalog
            .get_or_init(|| {
                self.catalog_initializations.fetch_add(1, Ordering::Relaxed);
                build_prefix_catalog(&self.spans)
            })
            .as_ref()
    }
}

fn build_prefix_catalog(spans: &[ModelMacroSpan]) -> Option<PrefixCatalog> {
    let mut definitions = Vec::with_capacity(spans.len());
    for span in spans {
        definitions.push(PrefixMacroDefinition {
            span: span.clone(),
            analysis: OnceLock::new(),
        });
    }
    let mut definitions_by_name = HashMap::new();
    for (index, definition) in definitions.iter().enumerate() {
        definitions_by_name
            .entry(definition.span.name.clone())
            .or_insert_with(Vec::new)
            .push(index);
    }
    Some(PrefixCatalog {
        definitions,
        definitions_by_name,
    })
}

impl PrefixMacroDefinition {
    fn analysis(&self, source: &str) -> Option<&PrefixMacroAnalysis> {
        self.analysis
            .get_or_init(|| build_prefix_macro_analysis(source, &self.span))
            .as_ref()
    }
}

fn build_prefix_macro_analysis(source: &str, span: &ModelMacroSpan) -> Option<PrefixMacroAnalysis> {
    let definition = source.get(span.start..span.end)?;
    let env = Environment::new();
    let template = env.template_from_str(definition).ok()?;
    let free_symbols = template.undeclared_variables(false);
    let uses_runtime_scalar = free_symbols
        .iter()
        .any(|symbol| RUNTIME_SCALAR_NAMES.contains(&symbol.as_str()));
    Some(PrefixMacroAnalysis {
        free_symbols,
        uses_runtime_scalar,
    })
}

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

fn valid_span(source: &str, span: &ModelMacroSpan) -> bool {
    span.start <= span.end
        && span.end <= source.len()
        && source.is_char_boundary(span.start)
        && source.is_char_boundary(span.end)
}

fn free_local_symbols(
    env: &Environment<'_>,
    source: &str,
    local_names: &HashSet<String>,
) -> Option<HashSet<String>> {
    let template = env.template_from_str(source).ok()?;
    Some(
        template
            .undeclared_variables(false)
            .into_iter()
            .filter(|symbol| local_names.contains(symbol))
            .collect(),
    )
}

fn macro_dependencies_with_status(
    env: &Environment<'_>,
    source: &str,
    macro_name: &str,
    local_names: &HashSet<String>,
) -> Option<(HashSet<String>, bool, bool)> {
    if !local_names
        .iter()
        .any(|name| name != macro_name && source.contains(name))
        && !RUNTIME_SCALAR_NAMES
            .iter()
            .any(|name| source.contains(name))
    {
        return Some((HashSet::new(), false, false));
    }
    let template = env.template_from_str(source).ok()?;
    let undeclared = template.undeclared_variables(false);
    let dependencies = undeclared
        .iter()
        .filter(|symbol| local_names.contains(*symbol))
        .cloned()
        .collect();
    let uses_runtime_scalar = undeclared
        .iter()
        .any(|symbol| RUNTIME_SCALAR_NAMES.contains(&symbol.as_str()));
    Some((dependencies, true, uses_runtime_scalar))
}

#[cfg(test)]
fn local_macro_dependencies_with_status(
    env: &Environment<'_>,
    source: &str,
    macro_name: &str,
    local_names: &HashSet<String>,
) -> Option<(HashSet<String>, bool)> {
    macro_dependencies_with_status(env, source, macro_name, local_names)
        .map(|(dependencies, compiled, _)| (dependencies, compiled))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_transitive_and_alias_reachability() {
        let sql = r#"
            {% macro inner() %}{{ ref('inner') }}{% endmacro %}
            {% macro outer() %}{{ inner() }}{% endmacro %}
            {% set alias = outer %}
            {{ alias() }}
        "#;
        let reachable = reachable_local_macros_with_prefix(sql, "", None, None).unwrap();
        assert_eq!(reachable.0, HashSet::from(["inner".into(), "outer".into()]));
    }

    #[test]
    fn excludes_unused_definitions() {
        let sql = r#"
            {% macro used() %}{{ ref('used') }}{% endmacro %}
            {% macro unused() %}{{ ref('unused') }}{% endmacro %}
            {{ used() }}
        "#;
        let reachable = reachable_local_macros_with_prefix(sql, "", None, None).unwrap();
        assert_eq!(reachable.0, HashSet::from(["used".into()]));
    }

    #[test]
    fn returns_empty_for_model_without_local_macros() {
        let (reachable, local_macro_count) =
            reachable_local_macros_with_prefix("SELECT 1", "", None, None).unwrap();
        assert!(reachable.is_empty());
        assert_eq!(local_macro_count, 0);
    }

    #[test]
    fn reuses_known_model_roots_without_compiling_model_source() {
        let sql = r#"
            {% macro inner() %}{% endmacro %}
            {% macro outer() %}{{ inner() }}{% endmacro %}
            {{ outer() }}
        "#;
        let roots = HashSet::from(["outer".to_owned()]);
        let (reachable, _local_macro_count, root_compile_performed, _definition_compile_count) =
            reachable_local_macros_with_status_and_prefix(sql, "", None, Some(&roots)).unwrap();
        assert_eq!(reachable, HashSet::from(["inner".into(), "outer".into()]));
        assert!(!root_compile_performed);
    }

    #[test]
    fn reuses_known_prefix_roots_without_compiling_model_source() {
        let prefix = r#"
            {% macro choose() %}{{ left() }}{% endmacro %}
        "#;
        let sql = r#"
            {% macro left() %}{{ ref('left') }}{% endmacro %}
            {% macro right() %}{{ ref('right') }}{% endmacro %}
            {{ choose() }}
        "#;
        let roots = HashSet::from(["choose".to_owned()]);
        let (reachable, _count, root_compile_performed, _definition_compile_count) =
            reachable_local_macros_with_status_and_prefix(sql, prefix, None, Some(&roots)).unwrap();
        assert_eq!(reachable, HashSet::from(["left".into()]));
        assert!(!root_compile_performed);
    }

    #[test]
    fn follows_higher_order_collection_and_conditional_symbols() {
        let sql = r#"
            {% macro first() %}{% endmacro %}
            {% macro second() %}{% endmacro %}
            {% macro invoke(callback) %}{{ callback() }}{% endmacro %}
            {% set callbacks = [first, second] %}
            {% set selected = first if execute else second %}
            {{ invoke(callbacks[0]) }}{{ selected() }}
        "#;
        let reachable = reachable_local_macros_with_prefix(sql, "", None, None).unwrap();
        assert_eq!(
            reachable.0,
            HashSet::from(["first".into(), "second".into(), "invoke".into()])
        );
    }

    #[test]
    fn delegates_shadowing_to_minijinja_free_symbol_analysis() {
        let sql = r#"
            {% macro shadowed() %}{{ ref('shadowed') }}{% endmacro %}
            {% macro caller() %}
                {% set shadowed = 'local value' %}{{ shadowed }}
            {% endmacro %}
            {{ caller() }}
        "#;
        let reachable = reachable_local_macros_with_prefix(sql, "", None, None).unwrap();
        assert_eq!(reachable.0, HashSet::from(["caller".into()]));
    }

    #[test]
    fn reports_uncompilable_source_as_unknown() {
        let sql = "{% macro broken() %}{{ ref('unfinished') }}";
        assert!(reachable_local_macros_with_prefix(sql, "", None, None).is_none());
    }

    #[test]
    fn reports_reachable_definition_compile_failure_as_unknown() {
        let sql = "{% bad other %}{{ broken() }}{% bad_other %}";
        let spans = [
            ModelMacroSpan {
                start: 0,
                end: 15,
                opening_end: 0,
                closing_start: 15,
                name: "broken".to_owned(),
            },
            ModelMacroSpan {
                start: 29,
                end: sql.len(),
                opening_end: 29,
                closing_start: sql.len(),
                name: "other".to_owned(),
            },
        ];
        assert!(reachable_local_macros_with_prefix(sql, "", Some(&spans), None).is_none());
    }

    #[test]
    fn skips_dense_inert_definitions_without_local_symbol_candidates() {
        let env = Environment::new();
        let local_names: HashSet<String> = (0..128)
            .map(|index| format!("runtime_macro_{index:03}"))
            .collect();
        let source = "{% macro runtime_macro_127() %}unused{% endmacro %}";
        let (dependencies, compiled) =
            local_macro_dependencies_with_status(&env, source, "runtime_macro_127", &local_names)
                .unwrap();
        assert!(dependencies.is_empty());
        assert!(!compiled);
    }

    #[test]
    fn unused_uncompilable_prefix_definition_does_not_poison_reachable_analysis() {
        let source = "{% macro used() %}used{% endmacro %}\n".to_owned()
            + "{% macro unused() %}{% invalid_statement %}{% endmacro %}";
        let prepared = PreparedMacroPrefix::new(&source);
        let roots = HashSet::from(["used".to_owned()]);
        let plan =
            macro_reachability_with_prepared_prefix("{{ used() }}", &prepared, None, Some(&roots))
                .unwrap();
        assert_eq!(plan.prefix_scopes, HashSet::from(["used".to_owned()]));
        assert_eq!(prepared.initialized_definition_count(), 1);
    }

    #[test]
    fn compiles_when_default_argument_references_another_macro() {
        let env = Environment::new();
        let local_names = HashSet::from(["caller".to_owned(), "default_value".to_owned()]);
        let source = "{% macro caller(value=default_value()) %}{{ value }}{% endmacro %}";
        let (dependencies, compiled) =
            local_macro_dependencies_with_status(&env, source, "caller", &local_names).unwrap();
        assert_eq!(dependencies, HashSet::from(["default_value".to_owned()]));
        assert!(compiled);
    }

    #[test]
    fn substring_collision_remains_conservative() {
        let env = Environment::new();
        let local_names = HashSet::from(["a".to_owned(), "caller".to_owned()]);
        let source = "{% macro caller() %}a label{% endmacro %}";
        let (_, compiled) =
            local_macro_dependencies_with_status(&env, source, "caller", &local_names).unwrap();
        assert!(compiled);
    }

    #[test]
    fn skips_all_definition_expansion_when_every_macro_is_a_root() {
        let mut sql = String::new();
        let mut roots = HashSet::new();
        for index in 0..128 {
            let name = format!("runtime_macro_{index:03}");
            roots.insert(name.clone());
            sql.push_str(&format!(
                "{{% macro {name}() %}}{{{{ ref('model_{index:03}') }}}}{{% endmacro %}}\n"
            ));
        }

        let (reachable, local_macro_count, root_compiled, definition_compiles) =
            reachable_local_macros_with_status_and_prefix(&sql, "", None, Some(&roots)).unwrap();
        assert_eq!(reachable, roots);
        assert_eq!(local_macro_count, 128);
        assert!(!root_compiled);
        assert_eq!(definition_compiles, 0);
    }

    #[test]
    fn keeps_partial_reachability_scoped_against_all_local_macros() {
        let mut sql = String::new();
        for index in 0..128 {
            sql.push_str(&format!(
                "{{% macro runtime_macro_{index:03}() %}}unused{{% endmacro %}}\n"
            ));
        }
        sql.push_str("{{ runtime_macro_000() }}{{ runtime_macro_001() }}{{ runtime_macro_002() }}");

        let (reachable, local_macro_count) =
            reachable_local_macros_with_prefix(&sql, "", None, None).unwrap();
        assert_eq!(local_macro_count, 128);
        assert_eq!(reachable.len(), 3);
        assert!(
            reachable
                .iter()
                .all(|name| name.starts_with("runtime_macro_00"))
        );
    }
}
