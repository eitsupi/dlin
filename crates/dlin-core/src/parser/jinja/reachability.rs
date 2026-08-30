use std::collections::{HashMap, HashSet, VecDeque};

use minijinja::Environment;

use super::source::{
    ModelMacroSpan, model_macro_definition_spans, strip_macro_definitions_for_runtime_analysis,
};

/// Compute the local macro definitions that can be reached from the model
/// source using MiniJinja's free-symbol analysis.
///
/// This intentionally does not parse call syntax. MiniJinja already accounts
/// for aliases, higher-order values, collections, conditional assignments,
/// and lexical shadowing when it computes undeclared variables. A failed
/// compile means that the provenance is unknown, so callers must use the
/// conservative whole-model fallback instead.
pub(crate) fn reachable_local_macros(
    sql: &str,
    known_spans: Option<&[ModelMacroSpan]>,
    known_roots: Option<&HashSet<String>>,
) -> Option<(HashSet<String>, usize)> {
    reachable_local_macros_with_status(sql, known_spans, known_roots)
        .map(|(reachable, local_macro_count, _, _)| (reachable, local_macro_count))
}

fn reachable_local_macros_with_status(
    sql: &str,
    known_spans: Option<&[ModelMacroSpan]>,
    known_roots: Option<&HashSet<String>>,
) -> Option<(HashSet<String>, usize, bool, usize)> {
    let spans = known_spans.map_or_else(
        || model_macro_definition_spans(sql),
        <[ModelMacroSpan]>::to_vec,
    );
    if spans.iter().any(|span| {
        span.start > span.end
            || span.end > sql.len()
            || !sql.is_char_boundary(span.start)
            || !sql.is_char_boundary(span.end)
    }) {
        return None;
    }
    let local_names: HashSet<String> = spans.iter().map(|span| span.name.clone()).collect();
    let local_macro_count = local_names.len();
    let env = Environment::new();

    let (roots, root_compile_performed) = if let Some(roots) = known_roots {
        (
            roots
                .intersection(&local_names)
                .cloned()
                .collect::<HashSet<_>>(),
            false,
        )
    } else {
        let model_source = strip_macro_definitions_for_runtime_analysis(sql, &spans);
        (free_local_symbols(&env, &model_source, &local_names)?, true)
    };
    // Every local definition is already a root, so expanding definitions can
    // add no names. The non-zero guard keeps zero-macro results scoped-empty.
    if local_macro_count > 0 && roots.len() == local_macro_count {
        return Some((roots, local_macro_count, root_compile_performed, 0));
    }
    // Partial traversal needs indexed definitions so each reachable name is
    // expanded in O(number of definitions for that name), not O(all spans).
    let definitions_by_name = spans.iter().enumerate().fold(
        HashMap::<String, Vec<usize>>::new(),
        |mut definitions, (index, span)| {
            definitions
                .entry(span.name.clone())
                .or_default()
                .push(index);
            definitions
        },
    );
    let mut reachable = roots;
    let mut pending: VecDeque<String> = reachable.iter().cloned().collect();
    let mut expanded = HashSet::new();
    let mut definition_compile_count = 0;
    while let Some(name) = pending.pop_front() {
        if !expanded.insert(name.clone()) {
            continue;
        }
        // Compile only definitions that are reachable from the model. If a
        // name is duplicated, conservatively union every definition under it.
        for index in definitions_by_name.get(&name).into_iter().flatten() {
            let span = &spans[*index];
            let definition = &sql[span.start..span.end];
            let (dependencies, compiled) =
                local_macro_dependencies_with_status(&env, definition, &name, &local_names)?;
            definition_compile_count += usize::from(compiled);
            for dependency in dependencies {
                if reachable.insert(dependency.clone()) {
                    pending.push_back(dependency);
                }
            }
        }
    }
    Some((
        reachable,
        local_macro_count,
        root_compile_performed,
        definition_compile_count,
    ))
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

fn local_macro_dependencies_with_status(
    env: &Environment<'_>,
    source: &str,
    macro_name: &str,
    local_names: &HashSet<String>,
) -> Option<(HashSet<String>, bool)> {
    if !local_names
        .iter()
        .any(|name| name != macro_name && source.contains(name))
    {
        return Some((HashSet::new(), false));
    }
    free_local_symbols(env, source, local_names).map(|dependencies| (dependencies, true))
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
        let reachable = reachable_local_macros(sql, None, None).unwrap();
        assert_eq!(reachable.0, HashSet::from(["inner".into(), "outer".into()]));
    }

    #[test]
    fn excludes_unused_definitions() {
        let sql = r#"
            {% macro used() %}{{ ref('used') }}{% endmacro %}
            {% macro unused() %}{{ ref('unused') }}{% endmacro %}
            {{ used() }}
        "#;
        let reachable = reachable_local_macros(sql, None, None).unwrap();
        assert_eq!(reachable.0, HashSet::from(["used".into()]));
    }

    #[test]
    fn returns_empty_for_model_without_local_macros() {
        let (reachable, local_macro_count) =
            reachable_local_macros("SELECT 1", None, None).unwrap();
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
            reachable_local_macros_with_status(sql, None, Some(&roots)).unwrap();
        assert_eq!(reachable, HashSet::from(["inner".into(), "outer".into()]));
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
        let reachable = reachable_local_macros(sql, None, None).unwrap();
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
        let reachable = reachable_local_macros(sql, None, None).unwrap();
        assert_eq!(reachable.0, HashSet::from(["caller".into()]));
    }

    #[test]
    fn reports_uncompilable_source_as_unknown() {
        let sql = "{% macro broken() %}{{ ref('unfinished') }}";
        assert!(reachable_local_macros(sql, None, None).is_none());
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
        assert!(reachable_local_macros(sql, Some(&spans), None).is_none());
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
            reachable_local_macros_with_status(&sql, None, Some(&roots)).unwrap();
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

        let (reachable, local_macro_count) = reachable_local_macros(&sql, None, None).unwrap();
        assert_eq!(local_macro_count, 128);
        assert_eq!(reachable.len(), 3);
        assert!(
            reachable
                .iter()
                .all(|name| name.starts_with("runtime_macro_00"))
        );
    }
}
