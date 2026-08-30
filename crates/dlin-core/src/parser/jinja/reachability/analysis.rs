use std::collections::HashSet;

use minijinja::Environment;

use super::super::source::ModelMacroSpan;
use super::catalog::RUNTIME_SCALAR_NAMES;

pub(super) fn valid_span(source: &str, span: &ModelMacroSpan) -> bool {
    span.start <= span.end
        && span.end <= source.len()
        && source.is_char_boundary(span.start)
        && source.is_char_boundary(span.end)
}

pub(super) fn free_local_symbols(
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

pub(super) fn macro_dependencies_with_status(
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
pub(super) fn local_macro_dependencies_with_status(
    env: &Environment<'_>,
    source: &str,
    macro_name: &str,
    local_names: &HashSet<String>,
) -> Option<(HashSet<String>, bool)> {
    macro_dependencies_with_status(env, source, macro_name, local_names)
        .map(|(dependencies, compiled, _)| (dependencies, compiled))
}
