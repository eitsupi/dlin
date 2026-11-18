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
fn comparison_reports_config_only_differences_and_unreadable_files() {
    let tmp = tempfile::tempdir().unwrap();
    let models = tmp.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(tmp.path().join("dbt_project.yml"), "name: p\n").unwrap();
    // The render fails on the unknown namespace and recovers the config by
    // regex, including the one in the branch known to be false.
    std::fs::write(
        models.join("a.sql"),
        "{% if var('flag', false) %}{{ config(materialized='table') }}{% endif %}{{ pkg.m() }}select 1",
    )
    .unwrap();
    std::fs::write(models.join("b.sql"), b"select '\xff'").unwrap();
    let report = super::compare::compare_project(tmp.path()).unwrap();
    assert_eq!(report.files, 1);
    assert_eq!(report.unreadable, vec!["models/b.sql"]);
    assert_eq!(
        report.config_differences.len(),
        1,
        "{:?}",
        report.config_differences
    );
    assert_eq!(report.identical_files, 0);
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
