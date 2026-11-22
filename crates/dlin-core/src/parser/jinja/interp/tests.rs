use std::collections::HashMap;
use std::path::PathBuf;

use super::*;
use crate::graph::dependency::{DependencyFunction, RuntimeValueSource};

fn library(macros: &[&str]) -> MacroLibrary {
    let files: Vec<(PathBuf, String)> = macros
        .iter()
        .enumerate()
        .map(|(i, source)| {
            (
                PathBuf::from(format!("macros/m{i}.sql")),
                source.to_string(),
            )
        })
        .collect();
    MacroLibrary::new(&files)
}

fn extract_with(
    sql: &str,
    macros: &[&str],
    vars: &[(&str, serde_json::Value)],
) -> DependencyExtraction {
    let vars: HashMap<String, serde_json::Value> = vars
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    extract_dependencies(sql, &library(macros), &vars)
}

fn extract(sql: &str) -> DependencyExtraction {
    extract_with(sql, &[], &[])
}

fn ref_names(extraction: &DependencyExtraction) -> Vec<&str> {
    let mut names: Vec<&str> = extraction.refs.iter().map(|r| r.name.as_str()).collect();
    names.sort();
    names
}

fn codes(extraction: &DependencyExtraction) -> Vec<&'static str> {
    let mut codes: Vec<&'static str> = extraction.reasons.iter().map(|r| r.code()).collect();
    codes.sort();
    codes.dedup();
    codes
}

// ----- plain dependencies ---------------------------------------------

#[test]
fn literal_refs_and_sources() {
    let e = extract(
        "select * from {{ ref('a') }} join {{ ref('pkg', 'b') }} join {{ source('raw', 't') }} \
         join {{ ref('c', version=2) }} join {{ ref('d', v='03') }}",
    );
    assert_eq!(ref_names(&e), vec!["a", "b", "c", "d"]);
    assert_eq!(e.refs[1].package.as_deref(), Some("pkg"));
    assert_eq!(e.refs[2].version.as_deref(), Some("2"));
    assert_eq!(e.refs[3].version.as_deref(), Some("3"));
    assert_eq!(e.sources.len(), 1);
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
}

#[test]
fn config_is_recorded() {
    let e = extract("{{ config(materialized='table', tags=['x', 'y']) }} select 1");
    assert_eq!(e.config.materialized.as_deref(), Some("table"));
    assert_eq!(e.config.tags, vec!["x", "y"]);
}

#[test]
fn rendered_unknown_values_are_not_reasons() {
    let e = extract("select * from {{ target.schema }}.t, {{ env_var('X') }}, {{ ref('a') }}");
    assert_eq!(ref_names(&e), vec!["a"]);
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
}

// ----- known values -------------------------------------------------------

#[test]
fn project_vars_and_defaults_select_dependencies() {
    let e = extract_with(
        "{% set kind = var('kind', 'x') %}select * from {{ ref('stg_' ~ kind) }} \
         join {{ ref(var('other')) }} join {{ ref(var('missing', default='z')) }}",
        &[],
        &[("other", serde_json::json!("y"))],
    );
    assert_eq!(ref_names(&e), vec!["stg_x", "y", "z"]);
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
}

#[test]
fn env_var_defaults_select_dependencies_without_reading_the_environment() {
    // `PATH` is set in any test environment; its value is never used.
    let e = extract_with(
        r#"
{{ ref('stg_' ~ env_var('PATH', 'x')) }}
{{ ref('stg_' ~ env_var('PATH', default='y')) }}
{{ ref(env_var('X', var('other'))) }}
{{ ref(env_var('X', var('missing', 'z'))) }}
"#,
        &[],
        &[("other", serde_json::json!("w"))],
    );
    assert_eq!(ref_names(&e), vec!["stg_x", "stg_y", "w", "z"]);
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
}

#[test]
fn env_var_defaults_keep_their_own_unknown_causes() {
    for (sql, code) in [
        (
            r#"{{ ref(env_var('X', var('missing'))) }}"#,
            "unresolved_var",
        ),
        (
            r#"{{ ref(env_var('X', env_var('Y'))) }}"#,
            "runtime_value_unknown",
        ),
        (
            r#"{{ ref(env_var(target.name, 'x')) }}"#,
            "runtime_value_unknown",
        ),
        // dbt does not allow secrets in models, so a default is not a value.
        (
            r#"{{ ref(env_var('DBT_ENV_SECRET_KIND', 'x')) }}"#,
            "runtime_value_unknown",
        ),
    ] {
        let e = extract(sql);
        assert!(e.refs.is_empty(), "{sql}: {:?}", e.refs);
        assert_eq!(codes(&e), vec![code], "{sql}");
    }
    let e = extract(r#"{{ ref(env_var('X', env_var('Y'))) }}"#);
    let DependencyReason::RuntimeValueUnknown { name, .. } = &e.reasons[0] else {
        panic!("{:?}", e.reasons);
    };
    assert_eq!(name.as_deref(), Some("Y"));
}

#[test]
fn known_false_branch_is_not_followed() {
    let e = extract(
        "{% if false %}{{ ref('never') }}{% endif %}\
         {% if execute %}{{ ref('only_at_run_time') }}{% endif %}\
         {% if var('flag', false) %}{{ ref('flagged') }}{% endif %}select 1",
    );
    assert!(e.refs.is_empty());
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
    assert!(
        e.decisions
            .iter()
            .all(|d| d.outcome == DecisionOutcome::SkippedKnownFalse)
    );
}

#[test]
fn is_incremental_follows_both_branches_without_reasons() {
    let e = extract("{% if is_incremental() %}{{ ref('a') }}{% else %}{{ ref('b') }}{% endif %}");
    assert_eq!(ref_names(&e), vec!["a", "b"]);
    assert!(e.reasons.is_empty());
}

#[test]
fn loops_over_known_lists() {
    let e = extract(
        "{% for k in ['a', 'b'] %}select * from {{ ref('stg_' ~ k) }}{% if not loop.last %} union all {% endif %}{% endfor %}",
    );
    assert_eq!(ref_names(&e), vec!["stg_a", "stg_b"]);
    assert!(e.reasons.is_empty());
}

#[test]
fn containers_and_namespaces_keep_known_elements() {
    let e = extract(
        "{% set names = [] %}{% do names.append('a') %}{% set ns = namespace(n='b') %}\
         {% set m = {'k': 'c'} %}\
         {{ ref(names[0]) }}{{ ref(ns.n) }}{{ ref(m['k']) }}{{ ref(m.get('k')) }}",
    );
    assert_eq!(ref_names(&e), vec!["a", "b", "c"]);
    assert!(e.reasons.is_empty());
}

#[test]
fn list_with_an_unknown_element_keeps_the_known_ones() {
    let e = extract("{% set l = ['a', env_var('X')] %}{{ ref(l[0]) }}");
    assert_eq!(ref_names(&e), vec!["a"]);
    assert!(e.reasons.is_empty());
}

// ----- placeholders never become dependencies ------------------------

#[test]
fn unknown_arguments_are_not_dependencies() {
    for (sql, code) in [
        ("{{ ref('stg_kind_' ~ var('kind')) }}", "unresolved_var"),
        (
            "{{ ref('stg_kind_' ~ target.name) }}",
            "runtime_value_unknown",
        ),
        ("{{ ref(env_var('M')) }}", "runtime_value_unknown"),
        ("{{ ref('a', version=var('v')) }}", "unresolved_var"),
        ("{{ source(var('s'), 't') }}", "unresolved_var"),
        (
            "{{ source('s', target.name | lower) }}",
            "runtime_value_unknown",
        ),
        (
            "{% set k = target.name %}{{ ref(k | string) }}",
            "runtime_value_unknown",
        ),
    ] {
        let e = extract(sql);
        assert!(
            e.refs.is_empty() && e.sources.is_empty(),
            "{sql}: {:?}",
            e.refs
        );
        assert_eq!(codes(&e), vec![code], "{sql}");
        assert!(
            e.decisions
                .iter()
                .any(|d| d.outcome == DecisionOutcome::Rejected),
            "{sql}"
        );
    }
}

#[test]
fn placeholder_from_a_macro_return_is_not_a_dependency() {
    let e = extract_with(
        "{% set kind = pick_kind() %}select * from {{ ref('stg_kind_' ~ kind) }}",
        &["{% macro pick_kind() %}{{ return(target.name) }}{% endmacro %}"],
        &[],
    );
    assert!(e.refs.is_empty());
    let reason = e
        .reasons
        .iter()
        .find(|r| r.code() == "runtime_value_unknown")
        .unwrap();
    let DependencyReason::RuntimeValueUnknown { source, name, at } = reason else {
        unreachable!()
    };
    assert_eq!(*source, RuntimeValueSource::TargetAttribute);
    assert_eq!(name.as_deref(), Some("target.name"));
    // Located at the call in the model, not inside the macro.
    assert_eq!(at.expr.as_deref(), Some("pick_kind()"));
}

#[test]
fn literal_ref_with_the_same_name_as_a_placeholder_still_resolves() {
    let e = extract("{{ ref('stg_kind_') }} {{ ref('__dbt_var_unknown__') }}");
    assert_eq!(ref_names(&e), vec!["__dbt_var_unknown__", "stg_kind_"]);
}

#[test]
fn shadowed_ref_is_not_a_dependency() {
    let e = extract(
        "{% set ref = target.name %}{{ ref }}{% macro m(ref) %}{{ ref }}{% endmacro %}{{ m('x') }}",
    );
    assert!(e.refs.is_empty());
}

#[test]
fn invalid_ref_call() {
    let e = extract("{{ ref() }}{{ ref(1) }}{{ source('only_one') }}");
    assert!(e.refs.is_empty() && e.sources.is_empty());
    assert_eq!(codes(&e), vec!["invalid_dependency_call"]);
}

// ----- unknown conditions ------------------------------------------------

#[test]
fn literal_ref_under_unknown_condition_is_kept_with_a_reason() {
    let e = extract("{% if var('flag') %}{{ ref('A') }}{% endif %}{{ ref('B') }}");
    assert_eq!(ref_names(&e), vec!["A", "B"]);
    assert_eq!(codes(&e), vec!["unresolved_var"]);
    let conditional: Vec<_> = e
        .decisions
        .iter()
        .filter(|d| d.outcome == (DecisionOutcome::Accepted { conditional: true }))
        .map(|d| d.literals[0].as_str())
        .collect();
    assert_eq!(conditional, vec!["A"]);
}

#[test]
fn unknown_condition_without_dependencies_is_not_a_reason() {
    let e = extract("{% if target.name == 'prod' %} limit 100 {% endif %}{{ ref('a') }}");
    assert_eq!(ref_names(&e), vec!["a"]);
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
}

#[test]
fn values_merged_after_an_unknown_branch_are_unknown() {
    let e = extract(
        "{% set k = 'a' %}{% if target.name == 'prod' %}{% set k = 'b' %}{% endif %}{{ ref(k) }}",
    );
    assert!(e.refs.is_empty());
    assert_eq!(codes(&e), vec!["runtime_value_unknown"]);
}

#[test]
fn same_value_on_both_branches_stays_known() {
    let e = extract(
        "{% if target.name == 'prod' %}{% set k = 'a' %}{% else %}{% set k = 'a' %}{% endif %}{{ ref(k) }}",
    );
    assert_eq!(ref_names(&e), vec!["a"]);
    assert!(e.reasons.is_empty());
}

#[test]
fn alias_sees_updates_merged_from_unknown_branches() {
    let e = extract(
        "{% set l = ['a'] %}{% set alias = l %}\
         {% if target.name == 'x' %}{% do alias.append('b') %}{% endif %}\
         {% for n in l %}{{ ref(n) }}{% endfor %}",
    );
    // The list's length is unknown after the branch, so the loop selects
    // dependencies from an unknown list.
    assert!(e.refs.is_empty());
    assert_eq!(codes(&e), vec!["runtime_value_unknown"]);
}

#[test]
fn return_on_one_branch_does_not_run_the_rest() {
    let e = extract_with(
        "{{ ref(pick()) }}",
        &[
            "{% macro pick() %}{% if target.name == 'x' %}{{ return('a') }}{% endif %}{{ return('b') }}{% endmacro %}",
        ],
        &[],
    );
    assert!(e.refs.is_empty());
    assert_eq!(codes(&e), vec!["runtime_value_unknown"]);

    let e = extract_with(
        "{{ ref(pick()) }}",
        &[
            "{% macro pick() %}{% if true %}{{ return('a') }}{% endif %}{{ return('b') }}{% endmacro %}",
        ],
        &[],
    );
    assert_eq!(ref_names(&e), vec!["a"]);
}

// ----- loops -----------------------------------------------------------

#[test]
fn unknown_loop_reaches_a_fixed_point() {
    let e = extract(
        "{% set ns = namespace(items=[]) %}\
         {% for r in run_query('select 1') %}{% do ns.items.append(r) %}{% endfor %}\
         {% for n in ns.items %}{{ ref(n) }}{% endfor %}{{ ref('fixed') }}",
    );
    assert_eq!(ref_names(&e), vec!["fixed"]);
    assert_eq!(codes(&e), vec!["runtime_value_unknown"]);
}

#[test]
fn literal_ref_in_an_unknown_loop_is_kept() {
    let e = extract("{% for r in env_var('X') %}{{ ref('inside') }}{% endfor %}");
    assert_eq!(ref_names(&e), vec!["inside"]);
    assert_eq!(codes(&e), vec!["runtime_value_unknown"]);
}

// ----- macros -----------------------------------------------------------

#[test]
fn project_macros_are_expanded_with_their_arguments() {
    let e = extract_with(
        "select * from {{ ref_wrapper('a') }} join {{ ref_wrapper(name='b') }} join {{ defaulted() }}",
        &[
            "{% macro ref_wrapper(name) %}{{ ref(name) }}{% endmacro %}",
            "{% macro defaulted(n='c', full='stg_' ~ n) %}{{ ref(full) }}{% endmacro %}",
        ],
        &[],
    );
    assert_eq!(ref_names(&e), vec!["a", "b", "stg_c"]);
    assert!(e.reasons.is_empty());
}

#[test]
fn macro_output_can_be_a_value() {
    let e = extract_with(
        "{{ source(schema_name(), 't') }}",
        &["{% macro schema_name() %}raw{% endmacro %}"],
        &[],
    );
    assert_eq!(e.sources[0].source_name, "raw");
}

#[test]
fn local_macros_and_call_blocks() {
    let e = extract(
        "{% macro wrap() %}({{ caller() }}){% endmacro %}\
         {% call wrap() %}select * from {{ ref('inner') }}{% endcall %}",
    );
    assert_eq!(ref_names(&e), vec!["inner"]);
}

#[test]
fn statement_body_is_not_a_parse_time_dependency() {
    let e = extract(
        "{% call statement('main', fetch_result=True) %}select * from {{ ref('queried') }}{% endcall %}\
         {% set rows = run_query('select * from ' ~ ref('in_argument')) %}",
    );
    assert_eq!(ref_names(&e), vec!["in_argument"]);
}

#[test]
fn unused_macros_are_not_executed() {
    let e = extract_with(
        "{% macro local_unused() %}{{ ref('local') }}{% endmacro %}select 1",
        &["{% macro unused() %}{{ ref('nope') }}{% endmacro %}"],
        &[],
    );
    assert!(e.refs.is_empty());
}

#[test]
fn recursion_is_bounded() {
    let e = extract_with(
        "{{ loop_forever() }}",
        &["{% macro loop_forever() %}{{ loop_forever() }}{% endmacro %}"],
        &[],
    );
    assert_eq!(codes(&e), vec!["analysis_limit_reached"]);
}

// ----- calls that hide dependencies ------------------------------------

#[test]
fn dispatch_namespace_and_unknown_calls() {
    let e = extract_with(
        "select * from {{ adapter.dispatch('kind_x_relation')() }}\
         join {{ kind_x_relation() }}\
         join {{ my_project.plain() }}\
         join {{ unknown_macro(ref('arg')) }}",
        &[
            "{% macro kind_x_relation() %}{{ return(adapter.dispatch('kind_x_relation')()) }}{% endmacro %}",
        ],
        &[],
    );
    // The literal ref passed to the unknown macro is kept.
    assert_eq!(ref_names(&e), vec!["arg"]);
    assert_eq!(
        codes(&e),
        vec![
            "unresolved_call",
            "unsupported_dispatch",
            "unsupported_macro_namespace"
        ]
    );
    // The dispatch inside the wrapper is reported at the wrapper call.
    let at_wrapper = e.reasons.iter().any(|r| {
        matches!(r, DependencyReason::UnsupportedDispatch { at, .. }
            if at.expr.as_deref() == Some("kind_x_relation()"))
    });
    assert!(at_wrapper, "{:?}", e.reasons);
}

#[test]
fn unknown_call_clobbers_mutable_arguments() {
    let e = extract("{% set ns = namespace(name='a') %}{% do mutate(ns) %}{{ ref(ns.name) }}");
    assert!(e.refs.is_empty());
    assert_eq!(codes(&e), vec!["unresolved_call"]);
}

#[test]
fn ordinary_methods_are_not_calls_to_unknown_macros() {
    let e = extract(
        "{% set l = [] %}{% do l.append('a') %}{% set s = 'A B'.lower().split(' ') %}{{ ref(s[0] ~ l[0]) }}",
    );
    assert_eq!(ref_names(&e), vec!["aa"]);
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
}

#[test]
fn unsupported_statement_is_scanned_for_literal_dependencies() {
    let e = extract("{% include 'other.sql' %}{% block b %}{{ ref('in_block') }}{% endblock %}");
    assert_eq!(ref_names(&e), vec!["in_block"]);
    assert_eq!(codes(&e), vec!["unsupported_jinja_construct"]);
}

#[test]
fn parse_failure_recovers_literal_calls() {
    let e = extract("{{ ref('a') }} {% if %} {{ ref('b' ~ x) }}");
    assert_eq!(ref_names(&e), vec!["a"]);
    assert_eq!(codes(&e), vec!["jinja_render_failed"]);
}

#[test]
fn reasons_carry_the_model_call_site() {
    let e = extract("select 1\n{{ ref(var('m')) }}");
    let DependencyReason::UnresolvedVar { name, at } = &e.reasons[0] else {
        panic!("{:?}", e.reasons)
    };
    assert_eq!(name.as_deref(), Some("m"));
    assert_eq!(at.line, Some(2));
    assert_eq!(at.expr.as_deref(), Some("var('m')"));
    assert!(
        e.decisions
            .iter()
            .any(|d| d.function == DependencyFunction::Ref)
    );
}

#[test]
fn return_inside_a_set_block_returns_from_the_macro() {
    let e = extract_with(
        "{{ ref(pick()) }}{{ ref('after') }}",
        &[
            "{% macro pick() %}{% set ignored %}{{ return('a') }}{% endset %}{{ return('b') }}{% endmacro %}",
        ],
        &[],
    );
    assert_eq!(ref_names(&e), vec!["a", "after"]);
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
}

#[test]
fn set_block_captures_known_output() {
    let e = extract("{% set name %}stg_{{ 'a' }}{% endset %}{{ ref(name | trim) }}");
    assert_eq!(ref_names(&e), vec!["stg_a"]);
}

// ----- regressions from review --------------------------------------------

#[test]
fn conditional_expression_keeps_each_side_on_its_own_state() {
    let e = extract(
        "{% set xs = [] %}{% set ignored = xs.append('A') if var('flag') else xs.append('B') %}{{ ref(xs[-1]) }}",
    );
    assert!(e.refs.is_empty(), "{:?}", e.refs);
    assert_eq!(codes(&e), vec!["unresolved_var"]);
}

#[test]
fn short_circuit_right_side_may_not_run() {
    let e = extract(
        "{% set xs = ['A'] %}{% set ignored = var('flag') and xs.append('B') %}{{ ref(xs[-1]) }}",
    );
    assert!(e.refs.is_empty(), "{:?}", e.refs);
    assert_eq!(codes(&e), vec!["unresolved_var"]);
}

#[test]
fn template_macro_reads_variables_when_called() {
    let e = extract(
        "{% set x = 'A' %}{% macro pick() %}{{ return(x) }}{% endmacro %}{% set x = 'B' %}{{ ref(pick()) }}",
    );
    assert_eq!(ref_names(&e), vec!["B"]);
}

#[test]
fn template_macro_calling_another_sees_the_template_variables() {
    let e = extract(
        "{% macro inner() %}{{ return(name) }}{% endmacro %}{% macro outer() %}{{ ref(inner()) }}{% endmacro %}\
         {% set name = 'A' %}{{ outer() }}",
    );
    assert_eq!(ref_names(&e), vec!["A"]);
}

#[test]
fn macro_stored_in_a_namespace_is_called() {
    let e = extract(
        "{% macro m() %}{{ ref('A') }}{% endmacro %}{% set ns = namespace(f=m) %}{{ ns.f() }}",
    );
    assert_eq!(ref_names(&e), vec!["A"]);
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
}

#[test]
fn method_on_a_possibly_callable_value_is_a_reason_even_if_unused() {
    let e = extract_with(
        "{% do pick().method() %}select 1",
        &["{% macro pick() %}{{ return(adapter.dispatch('pick')()) }}{% endmacro %}"],
        &[],
    );
    assert_eq!(codes(&e), vec!["unsupported_dispatch"]);
}

#[test]
fn methods_of_unknown_data_are_not_reasons() {
    let e = extract(
        "{% set name = (ref('t') | string) | lower %}{% do var('obj').method() %}\
         {% if name.endswith('traces') %}x{% endif %}{{ ref('a') }}",
    );
    assert_eq!(ref_names(&e), vec!["a", "t"]);
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
}

#[test]
fn unknown_keyword_splat_rejects_the_ref() {
    let e = extract("{{ ref('A', **var('opts')) }}");
    assert!(e.refs.is_empty());
    assert_eq!(codes(&e), vec!["unresolved_var"]);
    let e = extract("{{ ref('A', unexpected=1) }}{{ source('s', 't', x=1) }}");
    assert!(e.refs.is_empty() && e.sources.is_empty());
    assert_eq!(codes(&e), vec!["invalid_dependency_call"]);
}

#[test]
fn minijinja_functions_receive_keyword_arguments() {
    let e = extract("{% set d = dict(name='A') %}{{ ref(d.name) }}");
    assert_eq!(ref_names(&e), vec!["A"]);
}

#[test]
fn default_filter_follows_minijinja() {
    let e = extract(
        "{{ ref('' | default('A', true)) }}{{ ref(undefined_name | default('B')) }}{{ ref('C' | default('X')) }}",
    );
    assert_eq!(ref_names(&e), vec!["A", "B", "C"]);
}

#[test]
fn cyclic_containers_do_not_hang() {
    let e = extract("{% set xs = [] %}{% do xs.append(xs) %}{{ ref(xs | string) }}");
    assert!(e.refs.is_empty());
    assert_eq!(codes(&e), vec!["unsupported_jinja_construct"]);
}

#[test]
fn delegated_operations_have_a_budget() {
    let e = extract("{{ ref('a' * 100000000) }}");
    assert!(e.refs.is_empty());
    assert_eq!(codes(&e), vec!["unsupported_jinja_construct"]);
}

#[test]
fn loop_filter_decides_else_and_loop_info() {
    let e = extract("{% for x in [1] if false %}{% else %}{{ ref('A') }}{% endfor %}");
    assert_eq!(ref_names(&e), vec!["A"]);
    let e = extract(
        "{% for x in ['a', 'skip', 'b'] if x != 'skip' %}{% if loop.last %}{{ ref(x) }}{% endif %}{% endfor %}",
    );
    assert_eq!(ref_names(&e), vec!["b"]);
    let e = extract(
        "{% for x in ['a'] if var('f') %}{{ ref(x) }}{% else %}{{ ref('none') }}{% endfor %}",
    );
    assert_eq!(ref_names(&e), vec!["a", "none"]);
    assert_eq!(codes(&e), vec!["unresolved_var"]);
}

#[test]
fn default_filter_with_a_keyword_fallback() {
    let e = extract(
        "{{ ref(undefined_name | default(default_value='B')) }}{{ ref('' | default('C', boolean=true)) }}",
    );
    assert_eq!(ref_names(&e), vec!["B", "C"]);
}

#[test]
fn loop_filter_with_side_effects_is_not_followed_precisely() {
    let e = extract(
        "{% set xs = [] %}{% for x in ['a'] if xs.append('A') or true %}{% endfor %}{{ ref(xs[0]) }}",
    );
    assert!(e.refs.is_empty(), "{:?}", e.refs);
    assert_eq!(codes(&e), vec!["unsupported_jinja_construct"]);
}

#[test]
fn default_filter_keeps_non_plain_values() {
    let e = extract(
        "{% set r = ref('a') | default(default_value='x', boolean=true) %}{% set l = [] | default(['b'], true) %}\
         {% set m = ['c'] | default('y') %}{{ ref(l[0]) }}{{ ref(m[0]) }}{% if r %}{{ ref('d') }}{% endif %}",
    );
    // The empty list is falsy, so `boolean` replaces it.
    assert_eq!(ref_names(&e), vec!["a", "b", "c", "d"]);
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
}

#[test]
fn methods_of_known_values_follow_minijinja_pycompat() {
    let e = extract(
        "{{ ref('aaa'.replace('a', 'b', 1)) }}{{ ref('xxyxx'.strip('x')) }}{{ ref('a,b,c'.split(',', 1)[1]) }}\
         {{ ref({'k': 'd'}.get('k')) }}",
    );
    assert_eq!(ref_names(&e), vec!["b,c", "baa", "d", "y"]);
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
}

#[test]
fn unmodelled_method_arguments_make_the_value_unknown() {
    let e = extract("{{ ref('abc'.no_such_method()) }}");
    assert!(e.refs.is_empty());
    assert_eq!(codes(&e), vec!["unsupported_jinja_construct"]);
}

#[test]
fn repeated_dict_keys_keep_the_last_value() {
    let e = extract(
        "{% set m = {'x': 'a', 'x': 'b'} %}{{ ref(m['x']) }}{% for k in m %}{{ ref(k) }}{% endfor %}",
    );
    assert_eq!(ref_names(&e), vec!["b", "x"]);
}

#[test]
fn calls_through_context_and_builtins_are_unknown_calls() {
    let e = extract("{% do context['some_macro']() %}{% do builtins.other() %}select 1");
    assert_eq!(codes(&e), vec!["unresolved_call"]);
    let e =
        extract("{% set rel = adapter.get_relation(this.database, this.schema, 't') %}select 1");
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
}

#[test]
fn unknown_loop_with_a_false_filter_does_not_run_the_body() {
    let e = extract("{% for x in env_var('ITEMS') if false %}{{ ref('never') }}{% endfor %}");
    assert!(e.refs.is_empty());
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
}

#[test]
fn unknown_loop_with_a_false_filter_runs_else_unconditionally() {
    let e = extract(
        "{% set ns = namespace(name='before') %}\
         {% for x in env_var('ITEMS') if false %}{{ ref('never') }}\
         {% else %}{{ ref('fallback') }}{% set ns.name = 'after' %}{% endfor %}\
         {{ ref(ns.name) }}",
    );
    assert_eq!(ref_names(&e), vec!["after", "fallback"]);
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
    assert!(e.decisions.iter().any(|decision| {
        decision
            .reference
            .as_ref()
            .is_some_and(|r| r.name == "fallback")
            && decision.outcome == DecisionOutcome::Accepted { conditional: false }
    }));
}

#[test]
fn loop_filter_side_effects_do_not_restore_known_container_contents() {
    for iterable in ["env_var('ITEMS')", "['item']"] {
        let sql = r#"
{% set names = ['before'] %}
{% set alias = names %}
{% for x in ITERABLE if names.append('after') and false %}
{% else %}{{ ref(alias[-1]) }}{% endfor %}
{{ ref(names[-1]) }}
{{ ref('independent') }}
"#
        .replace("ITERABLE", iterable);
        let e = extract(&sql);
        assert_eq!(ref_names(&e), vec!["independent"], "{iterable}: {e:?}");
        assert!(codes(&e).contains(&"unsupported_jinja_construct"));
    }
}

#[test]
fn unknown_loop_filter_side_effects_can_change_later_filter_results() {
    let e = extract(
        r#"
{% set flags = [true, false] %}
{% for x in env_var('ITEMS') if flags.pop() %}
{{ ref('body') }}
{% else %}{{ ref('fallback') }}{% endfor %}
"#,
    );
    assert_eq!(ref_names(&e), vec!["body", "fallback"]);
    assert!(codes(&e).contains(&"unsupported_jinja_construct"));
}

#[test]
fn config_follows_the_last_call_and_unknown_branches() {
    let e = extract(
        "{{ config(materialized='view') }}{{ config(materialized='table', tags=['a']) }}{{ config(tags=['b']) }}",
    );
    assert_eq!(e.config.materialized.as_deref(), Some("table"));
    assert_eq!(e.config.tags, vec!["a", "b"]);
    let e = extract(
        "{% if target.name == 'prod' %}{{ config(materialized='table') }}{% else %}{{ config(materialized='view') }}{% endif %}",
    );
    assert_eq!(e.config.materialized, None);
}

// ----- generic test definitions in macro files ------------------------

/// A `{% test %}` block in a macro file defines the macro `test_<name>`,
/// alongside the file's other macros, whatever its whitespace control,
/// layout and defaults.
#[test]
fn test_block_defines_a_test_macro() {
    let macros = library(&[r#"
{% macro threshold_model() %}{{ ref('thresholds') }}{% endmacro %}

{%- test is_positive(model, column_name) -%}
select * from {{ model }} where {{ column_name }} <= 0
{%- endtest %}

{%+ test
    is_below(model, column_name, max_value=100)
%}
select * from {{ model }} join {{ threshold_model() }} on {{ column_name }} > {{ max_value }}
{% endtest +%}
"#]);
    assert!(macros.unparsed.is_empty());
    for name in ["threshold_model", "test_is_positive", "test_is_below"] {
        assert!(macros.get(name).is_some(), "{name} is defined");
    }
    assert_eq!(macros.get("test_is_below").unwrap().defaults.len(), 1);

    let extraction = extract_dependencies(
        r#"{{ test_is_below(ref('orders'), 'amount') }}"#,
        &macros,
        &HashMap::new(),
    );
    assert_eq!(ref_names(&extraction), ["orders", "thresholds"]);
    assert!(extraction.reasons.is_empty());
}

/// Only the tags the tokenizer reads are rewritten: text that looks like a
/// `test` tag in a comment, a raw block or a string stays as written.
#[test]
fn test_tag_text_outside_tags_is_not_rewritten() {
    let macros = library(&[r#"
{# {% test in_comment(model) %}{% endtest %} #}
{% raw %}{% test in_raw(model) %}{% endtest %}{% endraw %}
{% macro quoted() %}{{ "{% test in_string(model) %}" }}{% endmacro %}
"#]);
    assert!(macros.unparsed.is_empty());
    assert!(macros.get("quoted").is_some());
    for name in ["test_in_comment", "test_in_raw", "test_in_string"] {
        assert!(macros.get(name).is_none(), "{name} is not defined");
    }

    let extraction = extract_dependencies(
        r#"select '{{ quoted() }}' as text"#,
        &macros,
        &HashMap::new(),
    );
    assert!(extraction.reasons.is_empty());
}

/// A malformed `test` block is still a file that cannot be parsed.
#[test]
fn unclosed_test_block_is_unparsed() {
    let macros = library(&[r#"{% test is_positive(model) %}select 1"#]);
    assert_eq!(macros.unparsed, [PathBuf::from("macros/m0.sql")]);
}

/// Spans inside a rewritten file point into the file as written.
#[test]
fn test_block_spans_point_into_the_file_as_written() {
    let source = r#"{% test is_positive(model) %}select * from {{ ref('thresholds') }}{% endtest %}
{% test is_even(model) %}{{ source('raw', 'numbers') }}{% endtest %}"#;
    let macros = library(&[source]);
    let call_text = |name: &str| {
        let def = macros.get(name).unwrap();
        let expr = def
            .body
            .iter()
            .find_map(|stmt| match stmt {
                ir::Stmt::Emit(expr) => Some(expr),
                _ => None,
            })
            .unwrap();
        let span = expr.span;
        assert_eq!(
            span.line,
            u32::try_from(source[..span.start as usize].lines().count().max(1)).unwrap()
        );
        &source[span.start as usize..span.end as usize]
    };
    assert_eq!(call_text("test_is_positive"), "ref('thresholds')");
    assert_eq!(call_text("test_is_even"), "source('raw', 'numbers')");
}

// ----- mutable containers whatever made them ---------------------------

/// A dict or a list behaves the same whether a literal, `dict()`, `var()`, a
/// filter or a method made it: a change is seen by every alias.
#[test]
fn containers_made_by_minijinja_track_changes() {
    let vars = [
        ("map", serde_json::json!({"n": "old"})),
        ("list", serde_json::json!(["old"])),
    ];
    for init in [
        r#"{'n': 'old'}"#,
        r#"dict(n='old')"#,
        r#"var('map')"#,
        r#"[{'n': 'old'}] | first"#,
    ] {
        let sql = format!(
            r#"{{% set a = {init} %}}{{% set b = a %}}{{% do a.update({{'n': 'new'}}) %}}{{{{ ref(b.n) }}}}"#
        );
        let e = extract_with(&sql, &[], &vars);
        assert_eq!(ref_names(&e), vec!["new"], "{sql}");
        assert!(e.reasons.is_empty(), "{sql}: {:?}", e.reasons);
    }
    for init in [
        r#"['old']"#,
        r#"var('list')"#,
        r#"['old'] | list"#,
        r#"'x,old'.split(',') | reject('eq', 'x') | list"#,
        r#"'old'.split(',')"#,
        r#"['x', 'old'][1:]"#,
        r#"[] + ['old']"#,
    ] {
        let sql = format!(
            r#"{{% set a = {init} %}}{{% set b = a %}}{{% do a.append('new') %}}{{{{ ref(b.pop(-1)) }}}}{{{{ ref(b[-1]) }}}}"#
        );
        let e = extract_with(&sql, &[], &vars);
        assert_eq!(ref_names(&e), vec!["new", "old"], "{sql}");
        assert!(e.reasons.is_empty(), "{sql}: {:?}", e.reasons);
    }
}

/// The reported case: `dict()` then `update` in a `do`.
#[test]
fn dict_update_in_do_selects_the_new_value() {
    let e = extract(
        r#"{% set names = dict(n='a') %}{% do names.update({'n': 'b'}) %}select * from {{ ref(names.n) }}"#,
    );
    assert_eq!(ref_names(&e), vec!["b"]);
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
}

/// Each `var()` call is a new copy of the configured value, and a default
/// is the value passed.
#[test]
fn var_values_are_copies_and_defaults_are_the_value_passed() {
    let e = extract_with(
        r#"{% set a = var('v') %}{% set b = var('missing', a) %}{% do b.update({'n': 'new'}) %}{{ ref(a.n) }}{{ ref(var('v').n) }}"#,
        &[],
        &[("v", serde_json::json!({"n": "old"}))],
    );
    assert_eq!(ref_names(&e), vec!["new", "old"]);
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
}

/// A container that a filter returns as it is, or keeps inside its result,
/// is the same container; a container the filter builds is a new one, even
/// when it is equal to an input.
#[test]
fn filter_results_keep_the_identity_of_their_input_containers() {
    let e = extract(
        r#"{% set child = {'n': 'old'} %}{% set y = [child] | first %}{% do y.update({'n': 'new'}) %}{{ ref(child.n) }}"#,
    );
    assert_eq!(ref_names(&e), vec!["new"]);
    let e = extract(
        r#"{% set child = {'n': 'old'} %}{% set y = ([child] | list)[0] %}{% do child.update(n='new') %}{{ ref(y.n) }}"#,
    );
    assert_eq!(ref_names(&e), vec!["new"]);
    let e = extract(
        r#"{% set x = ['old'] %}{% set y = ([x, 'old'] | batch(1) | list)[1] %}{% do y.append('new') %}{{ ref(x[-1]) }}"#,
    );
    assert_eq!(ref_names(&e), vec!["old"]);
    let e = extract(
        r#"{% set x = ['old'] %}{% set y = x | list %}{% do y.append('new') %}{{ ref(x[-1]) }}"#,
    );
    assert_eq!(ref_names(&e), vec!["old"]);
}

/// A loop over a filtered project variable still selects its dependencies.
#[test]
fn loops_over_filtered_vars_select_dependencies() {
    let e = extract_with(
        r#"{% for m in var('models') | selectattr('enabled') %}{{ ref(m.name) }}{% endfor %}"#,
        &[],
        &[(
            "models",
            serde_json::json!([
                {"name": "a", "enabled": true},
                {"name": "b", "enabled": false},
            ]),
        )],
    );
    assert_eq!(ref_names(&e), vec!["a"]);
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
}

/// Methods that change a container in place, as Fusion's minijinja has them.
#[test]
fn mutating_methods_change_the_container() {
    let e = extract(
        r#"{% set a = dict(n='old') %}{% set b = a.update({'n': 'new'}) %}{{ ref(b.pop('n')) }}{{ ref(a.get('n', 'gone')) }}{{ ref(a.pop('n', 'default')) }}"#,
    );
    assert_eq!(ref_names(&e), vec!["default", "gone", "new"]);
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
    let e = extract(
        r#"{% set xs = ['a', 'b', 'c'] %}{% do xs.pop(0) %}{% set ys = xs.insert(1, 'x') %}{{ ref(ys[0] ~ xs[1] ~ xs | length) }}{{ ref(xs.clear() | default('cleared')) }}{{ ref('n' ~ xs | length) }}"#,
    );
    assert_eq!(ref_names(&e), vec!["bx3", "cleared", "n0"]);
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
}

/// A change that is not modelled makes the container unknown instead of
/// leaving its old value, also when the result is discarded by `do`.
#[test]
fn unmodelled_changes_make_the_container_unknown() {
    for method in [
        "setdefault('n', 'new')",
        "popitem()",
        "update([('n', 'new')])",
    ] {
        let sql = format!(
            r#"{{% set a = dict(n='old') %}}{{% set b = a %}}{{% do a.{method} %}}{{{{ ref(b.n) }}}}"#
        );
        let e = extract(&sql);
        assert!(ref_names(&e).is_empty(), "{sql}: {:?}", ref_names(&e));
        assert_eq!(codes(&e), vec!["unsupported_jinja_construct"], "{sql}");
    }
    for method in ["remove('old')", "sort()", "reverse()"] {
        let sql = format!(r#"{{% set a = var('list') %}}{{% do a.{method} %}}{{{{ ref(a[0]) }}}}"#);
        let e = extract_with(&sql, &[], &[("list", serde_json::json!(["old", "x"]))]);
        assert!(ref_names(&e).is_empty(), "{sql}: {:?}", ref_names(&e));
        assert_eq!(codes(&e), vec!["unsupported_jinja_construct"], "{sql}");
    }
}

/// A project variable too large to model is unknown, not cut short.
#[test]
fn oversized_vars_reach_the_analysis_limit() {
    let items: Vec<String> = (0..=10_000).map(|i| format!("m{i}")).collect();
    let e = extract_with(
        r#"{{ ref(var('models')[0]) }}"#,
        &[],
        &[("models", serde_json::json!(items))],
    );
    assert!(ref_names(&e).is_empty(), "{:?}", ref_names(&e));
    assert_eq!(codes(&e), vec!["analysis_limit_reached"]);
}

/// `copy()` is a new outer container holding the same items; `range()` is
/// an iterable, not a list; a negative `insert` index is not modelled.
#[test]
fn copies_ranges_and_negative_inserts() {
    let e = extract(
        r#"{% set child = {'n': 'old'} %}{% set a = [child] %}{% set b = a.copy() %}{% do b.append('x') %}{% do b[0].update(n='new') %}{{ ref(a | length ~ child.n) }}"#,
    );
    assert_eq!(ref_names(&e), vec!["1new"]);
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
    let e = extract(r#"{% set r = range(2) %}{% do r.append(9) %}{{ ref('m' ~ (r | last)) }}"#);
    assert_eq!(ref_names(&e), vec!["m1"]);
    let e = extract(r#"{% set xs = ['a', 'b'] %}{% do xs.insert(-1, 'x') %}{{ ref(xs[0]) }}"#);
    assert!(ref_names(&e).is_empty(), "{:?}", ref_names(&e));
    assert_eq!(codes(&e), vec!["unsupported_jinja_construct"]);
}

/// `chain` gives a view of its inputs in minijinja and does not exist in
/// Fusion: its result is unknown rather than a copy taken too early. Two
/// ranges concatenate into an iterable, not a list, and a concatenation
/// with a list is a new list. `dict()` keeps the order of its entries.
#[test]
fn views_concatenations_and_dict_order() {
    for sql in [
        r#"{% set d = {'n': 'a'} %}{% set merged = d | chain({}) %}{% do d.update({'n': 'b'}) %}{{ ref(merged.n) }}"#,
        r#"{% set xs = ['a'] %}{% set merged = xs | chain([]) %}{% do xs.append('b') %}{{ ref(merged | last) }}"#,
        r#"{% set x = {'n': 'old'} %}{% for y in [x] | chain(range(0)) %}{% do y.update({'n': 'new'}) %}{% endfor %}{{ ref(x.n) }}"#,
        // A splat argument is not modelled either.
        r#"{% set x = {'n': 'old'} %}{% set args = [[x]] %}{% for y in [] | chain(*args) %}{% do y.update({'n': 'new'}) %}{% endfor %}{{ ref(x.n) }}"#,
        // Nor is a filter minijinja does not know.
        r#"{% set x = {'n': 'old'} %}{% for y in [x] | no_such_filter %}{% do y.update({'n': 'new'}) %}{% endfor %}{{ ref(x.n) }}"#,
        // Also when another argument is unknown.
        r#"{% set x = {'n': 'old'} %}{% set ignored = x | no_such_filter(target.name) %}{{ ref(x.n) }}"#,
        r#"{% set x = {'n': 'old'} %}{% set ignored = x | no_such_filter(sep=target.name) %}{{ ref(x.n) }}"#,
    ] {
        let e = extract(sql);
        assert!(
            !ref_names(&e).contains(&"a") && !ref_names(&e).contains(&"old"),
            "{sql}: {:?}",
            ref_names(&e)
        );
        assert!(codes(&e).contains(&"unsupported_jinja_construct"), "{sql}");
    }
    let e = extract(
        r#"{% set xs = ['a'] %}{% set ys = xs + [] %}{% do xs.append('b') %}{{ ref(ys | last) }}"#,
    );
    assert_eq!(ref_names(&e), vec!["a"]);
    let e = extract(
        r#"{% set r = range(1) + range(1) %}{% do r.append(9) %}{{ ref('m' ~ (r | last)) }}"#,
    );
    assert_eq!(ref_names(&e), vec!["m0"]);
    let e =
        extract(r#"{{ ref(dict({'z': 1, 'a': 2}) | first) }}{{ ref(dict(z=1, a=2) | first) }}"#);
    assert_eq!(ref_names(&e), vec!["z"]);
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
}
