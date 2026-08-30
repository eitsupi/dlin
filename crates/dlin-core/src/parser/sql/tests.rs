use std::collections::HashSet;

use super::*;
// Access private fallback directly so the regex path is covered independently
// of the Jinja extractor (which normally runs first in extract_refs).
use super::extract_refs_regex;
use super::recovery::{
    extract_refs_and_sources_regex_scoped, extract_refs_and_sources_regex_spans,
};

#[test]
fn test_single_ref() {
    let sql = "SELECT * FROM {{ ref('stg_orders') }}";
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "stg_orders");
    assert!(refs[0].package.is_none());
}

#[test]
fn test_double_quoted_ref() {
    let sql = r#"SELECT * FROM {{ ref("stg_orders") }}"#;
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "stg_orders");
}

#[test]
fn test_two_arg_ref() {
    let sql = "SELECT * FROM {{ ref('other_project', 'stg_orders') }}";
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].package.as_deref(), Some("other_project"));
    assert_eq!(refs[0].name, "stg_orders");
}

#[test]
fn test_whitespace_control() {
    let sql = "SELECT * FROM {{- ref('stg_orders') -}}";
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "stg_orders");
}

#[test]
fn test_multiple_refs() {
    let sql = r#"
            SELECT
                o.*,
                c.name
            FROM {{ ref('stg_orders') }} o
            JOIN {{ ref('stg_customers') }} c ON o.customer_id = c.id
        "#;
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].name, "stg_orders");
    assert_eq!(refs[1].name, "stg_customers");
}

#[test]
fn test_source() {
    let sql = "SELECT * FROM {{ source('raw', 'orders') }}";
    let sources = extract_sources(sql);
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].source_name, "raw");
    assert_eq!(sources[0].table_name, "orders");
}

#[test]
fn test_source_whitespace_control() {
    let sql = "SELECT * FROM {{- source('raw', 'orders') -}}";
    let sources = extract_sources(sql);
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].source_name, "raw");
}

#[test]
fn test_strip_jinja_comments() {
    let sql = r#"
            {# This is a comment with {{ ref('should_be_ignored') }} #}
            SELECT * FROM {{ ref('actual_model') }}
        "#;
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "actual_model");
}

#[test]
fn test_mixed_refs_and_sources() {
    let sql = r#"
            SELECT *
            FROM {{ source('raw', 'orders') }}
            JOIN {{ ref('stg_customers') }} ON 1=1
        "#;
    let refs = extract_refs(sql);
    let sources = extract_sources(sql);
    assert_eq!(refs.len(), 1);
    assert_eq!(sources.len(), 1);
}

#[test]
fn test_no_refs() {
    let sql = "SELECT 1 as id";
    let refs = extract_refs(sql);
    assert!(refs.is_empty());
}

#[test]
fn test_extra_spaces() {
    let sql = "SELECT * FROM {{  ref(  'stg_orders'  )  }}";
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "stg_orders");
}

#[test]
fn test_ref_with_version_kwarg() {
    let sql = "SELECT * FROM {{ ref('my_model', version=2) }}";
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "my_model");
    assert_eq!(refs[0].version.as_deref(), Some("2"));
    assert!(refs[0].package.is_none());
}

#[test]
fn test_ref_with_version_kwarg_spaced() {
    let sql = "SELECT * FROM {{ ref('my_model', version = 3) }}";
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "my_model");
    assert_eq!(refs[0].version.as_deref(), Some("3"));
}

#[test]
fn test_ref_without_version_has_none() {
    let sql = "SELECT * FROM {{ ref('my_model') }}";
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].version, None);
}

#[test]
fn test_ref_two_arg_has_no_version() {
    let sql = "SELECT * FROM {{ ref('pkg', 'my_model') }}";
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].package.as_deref(), Some("pkg"));
    assert_eq!(refs[0].name, "my_model");
    assert_eq!(refs[0].version, None);
}

#[test]
fn test_version_does_not_conflict_with_two_arg_form() {
    // ref('pkg', 'name') must NOT match the version=N branch
    let sql = "SELECT * FROM {{ ref('mypkg', 'model_a') }}";
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].package.as_deref(), Some("mypkg"));
    assert_eq!(refs[0].name, "model_a");
    assert_eq!(refs[0].version, None);
}

#[test]
fn test_two_arg_ref_with_version_kwarg() {
    let sql = "SELECT * FROM {{ ref('mypkg', 'my_model', version=3) }}";
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].package.as_deref(), Some("mypkg"));
    assert_eq!(refs[0].name, "my_model");
    assert_eq!(refs[0].version.as_deref(), Some("3"));
}

#[test]
fn test_ref_with_v_shorthand_kwarg() {
    let sql = "SELECT * FROM {{ ref('my_model', v=2) }}";
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "my_model");
    assert_eq!(refs[0].version.as_deref(), Some("2"));
    assert!(refs[0].package.is_none());
}

#[test]
fn test_two_arg_ref_with_v_shorthand_kwarg() {
    let sql = "SELECT * FROM {{ ref('mypkg', 'my_model', v=3) }}";
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].package.as_deref(), Some("mypkg"));
    assert_eq!(refs[0].name, "my_model");
    assert_eq!(refs[0].version.as_deref(), Some("3"));
}

#[test]
fn test_ref_with_string_version_kwarg() {
    // dbt-core accepts version='alpha' for non-integer version strings
    let refs = extract_refs_regex("SELECT * FROM {{ ref('my_model', version='alpha') }}");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "my_model");
    assert_eq!(refs[0].version.as_deref(), Some("alpha"));
}

#[test]
fn test_ref_with_quoted_integer_version_kwarg() {
    // version='2' (quoted) must resolve identically to version=2 (bare integer)
    let refs = extract_refs_regex("SELECT * FROM {{ ref('my_model', version='2') }}");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].version.as_deref(), Some("2"));
}

#[test]
fn test_ref_with_padded_integer_version_kwarg() {
    // version='02' must normalize to "2" to match YAML v: 2 → version_value_to_str → "2"
    let refs = extract_refs_regex("SELECT * FROM {{ ref('my_model', version='02') }}");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].version.as_deref(), Some("2"));
}

#[test]
fn test_ref_with_decimal_version_kwarg() {
    // version='2.0' stays as "2.0" — matching YAML `v: "2.0"` which also keeps "2.0".
    // Both use i64-only normalization so non-integer numeric strings are not rewritten.
    let refs = extract_refs_regex("SELECT * FROM {{ ref('my_model', version='2.0') }}");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].version.as_deref(), Some("2.0"));
}

// These tests call the regex fallback directly to confirm `v=` support
// in that path, independent of the Jinja extractor.

#[test]
fn test_regex_fallback_v_shorthand_kwarg() {
    let refs = extract_refs_regex("SELECT * FROM {{ ref('my_model', v=2) }}");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "my_model");
    assert_eq!(refs[0].version.as_deref(), Some("2"));
    assert!(refs[0].package.is_none());
}

#[test]
fn test_regex_fallback_two_arg_v_shorthand_kwarg() {
    let refs = extract_refs_regex("SELECT * FROM {{ ref('mypkg', 'my_model', v=3) }}");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].package.as_deref(), Some("mypkg"));
    assert_eq!(refs[0].name, "my_model");
    assert_eq!(refs[0].version.as_deref(), Some("3"));
}

// ─── Refs nested inside macro arguments (GitHub issue: refs in macro args
//     were dropped when jinja rendering failed on an unknown macro) ───

#[test]
fn test_refs_in_namespaced_macro_args() {
    // Package macros (pkg.macro_name) can't be resolved by minijinja, so
    // rendering fails; refs in the arguments must still be extracted.
    let sql = r#"
            {{
                shared_macros.import_cte([
                    ('orders', ref('int_orders_aggregated')),
                    ('returns', ref('int_returns_by_region')),
                    ('customers', ref('dim_customers'))
                ])
            }}
            SELECT * FROM orders
        "#;
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 3);
    assert!(refs.iter().any(|r| r.name == "int_orders_aggregated"));
    assert!(refs.iter().any(|r| r.name == "int_returns_by_region"));
    assert!(refs.iter().any(|r| r.name == "dim_customers"));
}

#[test]
fn test_refs_in_unknown_macro_args() {
    let sql = "{{ import_cte(ref('upstream_model')) }}";
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "upstream_model");
}

#[test]
fn test_ref_kwarg_in_package_macro() {
    // dbt_utils is typically not part of the scanned project macros
    let sql = "SELECT {{ dbt_utils.star(from=ref('stg_orders')) }} FROM x";
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "stg_orders");
}

#[test]
fn test_ref_after_failed_macro_call_still_extracted() {
    // Rendering aborts at the unknown macro; refs after the failure point
    // must be recovered by the regex merge.
    let sql = r#"
            SELECT * FROM {{ ref('before_model') }}
            JOIN ({{ unknown_macro(ref('arg_model')) }}) USING (id)
            JOIN {{ ref('after_model') }} USING (id)
        "#;
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 3);
    assert!(refs.iter().any(|r| r.name == "before_model"));
    assert!(refs.iter().any(|r| r.name == "arg_model"));
    assert!(refs.iter().any(|r| r.name == "after_model"));
}

#[test]
fn test_dynamic_ref_salvaged_from_partial_render() {
    // ref(var(...)) cannot be found by regex; it must be salvaged from the
    // partial jinja render even though the template fails later.
    let sql = r#"
            SELECT * FROM {{ ref('model_' ~ var('env', 'dev')) }}
            JOIN ({{ unknown_macro() }}) USING (id)
        "#;
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "model_dev");
}

#[test]
fn test_runtime_env_var_branches_are_merged() {
    let sql = r#"
            {% if env_var('REGION') == 'us' %}
                SELECT * FROM {{ ref('orders_us') }}
            {% else %}
                SELECT * FROM {{ ref('orders_eu') }}
            {% endif %}
        "#;
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 2);
    assert!(refs.iter().any(|r| r.name == "orders_us"));
    assert!(refs.iter().any(|r| r.name == "orders_eu"));
}

#[test]
fn test_runtime_fallback_ignores_plus_controlled_raw_blocks() {
    let sql = r#"
            {%+ raw +%}{{ ref('not_a_dependency') }}{%+ endraw +%}
            {% if execute %}
                {{ ref('execute_true_raw_guard') }}
            {% else %}
                {{ ref('execute_false_raw_guard') }}
            {% endif %}
        "#;
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 2);
    assert!(refs.iter().any(|r| r.name == "execute_true_raw_guard"));
    assert!(refs.iter().any(|r| r.name == "execute_false_raw_guard"));
    assert!(!refs.iter().any(|r| r.name == "not_a_dependency"));
}

#[test]
fn test_execute_branches_are_merged() {
    let sql = r#"
            {% if execute %}
                SELECT * FROM {{ ref('execute_true') }}
            {% else %}
                SELECT * FROM {{ ref('execute_false') }}
            {% endif %}
        "#;
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 2);
    assert!(refs.iter().any(|r| r.name == "execute_true"));
    assert!(refs.iter().any(|r| r.name == "execute_false"));
}

#[test]
fn test_all_reachable_local_macros_use_whole_model_recovery() {
    let sql = r#"
            {% macro selected() %}
                {% if env_var('REGION') == 'us' %}{{ ref('selected_us') }}{% endif %}
            {% endmacro %}
            {% macro alternate() %}{{ ref('alternate') }}{% endmacro %}
            {% if execute %}{{ selected() }}{% else %}{{ alternate() }}{% endif %}
        "#;
    let outcome = super::super::jinja::extract_via_jinja(sql, "");
    assert!(outcome.complete && outcome.model_uncertain);
    assert!(regex_fallback_macro_scopes(sql, "", &outcome).is_none());
}

#[test]
fn test_partial_reachability_keeps_three_of_128_macros_scoped() {
    let mut sql = String::new();
    for index in 0..128 {
        sql.push_str(&format!(
            "{{% macro runtime_macro_{index:03}() %}}unused{{% endmacro %}}\n"
        ));
    }
    sql.push_str(
        "{% if execute %}{{ runtime_macro_000() }}{% else %}{{ runtime_macro_001() }}{% endif %}{{ runtime_macro_002() }}",
    );

    let outcome = super::super::jinja::extract_via_jinja(&sql, "");
    let scopes = regex_fallback_macro_scopes(&sql, "", &outcome).unwrap();
    assert_eq!(scopes.len(), 3);
    assert!(
        scopes
            .iter()
            .all(|name| name.starts_with("runtime_macro_00"))
    );
}

#[test]
fn test_zero_local_macros_remains_scoped_empty_for_model_recovery() {
    let sql = "{% if execute %}{{ ref('runtime_ref') }}{% else %}{{ ref('parse_ref') }}{% endif %}";
    let outcome = super::super::jinja::extract_via_jinja(sql, "");
    let scopes = regex_fallback_macro_scopes(sql, "", &outcome).unwrap();
    assert!(scopes.is_empty());
}

#[test]
fn test_top_level_uncertainty_recovers_alternate_macro_refs() {
    let sql = r#"
            {% macro selected() %}
                {% if env_var('REGION') == 'us' %}
                    {{ ref('selected_us_ref') }}
                {% else %}
                    {{ ref('selected_eu_ref') }}
                {% endif %}
            {% endmacro %}
            {% macro alternate() %}{{ ref('alternate_ref') }}{% endmacro %}
            {% if execute %}{{ selected() }}{% else %}{{ alternate() }}{% endif %}
        "#;
    let refs = extract_refs(sql);
    assert!(refs.iter().any(|r| r.name == "selected_us_ref"));
    assert!(refs.iter().any(|r| r.name == "selected_eu_ref"));
    assert!(refs.iter().any(|r| r.name == "alternate_ref"));
}

#[test]
fn test_top_level_env_var_uncertainty_recovers_alternate_macro_refs() {
    let sql = r#"
            {% macro selected() %}{{ ref('selected_env_ref') }}{% endmacro %}
            {% macro alternate() %}{{ ref('alternate_env_ref') }}{% endmacro %}
            {% if env_var('REGION') == 'us' %}{{ selected() }}{% else %}{{ alternate() }}{% endif %}
        "#;
    let refs = extract_refs(sql);
    assert!(refs.iter().any(|r| r.name == "selected_env_ref"));
    assert!(refs.iter().any(|r| r.name == "alternate_env_ref"));
}

#[test]
fn test_top_level_uncertainty_excludes_unreachable_local_macro_refs() {
    let sql = r#"
            {% macro selected() %}
                {% if env_var('REGION') == 'us' %}
                    {{ ref('selected_us') }}
                {% else %}
                    {{ ref('selected_eu') }}
                {% endif %}
            {% endmacro %}
            {% macro unused() %}{{ ref('unused_local') }}{% endmacro %}
            {{ selected() }}
        "#;
    let refs = extract_refs(sql);
    assert!(refs.iter().any(|r| r.name == "selected_us"));
    assert!(refs.iter().any(|r| r.name == "selected_eu"));
    assert!(!refs.iter().any(|r| r.name == "unused_local"));
}

#[test]
fn test_prefix_uncertainty_recovers_local_macro_callbacks_without_hint_scan() {
    let macro_prefix = r#"
            {% macro choose(left, right) %}
                {% if env_var('REGION') == 'us' %}{{ left() }}{% else %}{{ right() }}{% endif %}
            {% endmacro %}
        "#;
    let sql = r#"
            {% macro left() %}{{ ref('left_local') }}{% endmacro %}
            {% macro right() %}{{ ref('right_local') }}{% endmacro %}
            {{ choose(left, right) }}
        "#;
    let refs = extract_refs_and_sources(sql, macro_prefix).0;
    assert!(refs.iter().any(|r| r.name == "left_local"));
    assert!(refs.iter().any(|r| r.name == "right_local"));
}

#[test]
fn test_prefix_uncertainty_recovers_direct_local_macro_references() {
    let macro_prefix = r#"
            {% macro choose() %}
                {% if env_var('REGION') == 'us' %}{{ left() }}{% else %}{{ right() }}{% endif %}
            {% endmacro %}
        "#;
    let sql = r#"
            {% macro left() %}{{ ref('left_direct') }}{% endmacro %}
            {% macro right() %}{{ ref('right_direct') }}{% endmacro %}
            {{ choose() }}
        "#;
    let refs = extract_refs_and_sources(sql, macro_prefix).0;
    assert!(refs.iter().any(|r| r.name == "left_direct"));
    assert!(refs.iter().any(|r| r.name == "right_direct"));
}

#[test]
fn test_called_prefix_execute_branches_are_merged() {
    let macro_prefix = r#"
            {% macro choose() %}
                {% if execute %}{{ ref('prefix_execute_true') }}{% else %}{{ ref('prefix_execute_false') }}{% endif %}
            {% endmacro %}
        "#;
    let sql = "{{ choose() }}";
    let refs = extract_refs_and_sources(sql, macro_prefix).0;
    assert!(refs.iter().any(|r| r.name == "prefix_execute_true"));
    assert!(refs.iter().any(|r| r.name == "prefix_execute_false"));
}

#[test]
fn test_all_local_roots_still_reach_transitive_prefix_execute_macro() {
    let macro_prefix = r#"
            {% macro dispatch() %}{{ choose() }}{% endmacro %}
            {% macro choose() %}
                {% if execute %}{{ ref('prefix_all_roots_true') }}{% else %}{{ ref('prefix_all_roots_false') }}{% endif %}
            {% endmacro %}
        "#;
    let sql = r#"
            {% macro wrapper() %}{{ dispatch() }}{% endmacro %}
            {% macro other() %}{{ ref('other_all_roots') }}{% endmacro %}
            {{ wrapper() }}{{ other() }}
        "#;
    let refs = extract_refs_and_sources(sql, macro_prefix).0;
    assert!(refs.iter().any(|r| r.name == "prefix_all_roots_true"));
    assert!(refs.iter().any(|r| r.name == "prefix_all_roots_false"));
    assert!(refs.iter().any(|r| r.name == "other_all_roots"));
}

#[test]
fn test_transitive_prefix_execute_uncertainty_is_recovered() {
    let macro_prefix = r#"
            {% macro dispatch() %}{{ choose() }}{% endmacro %}
            {% macro choose() %}
                {% if execute %}{{ ref('prefix_transitive_true') }}{% else %}{{ ref('prefix_transitive_false') }}{% endif %}
            {% endmacro %}
        "#;
    let refs = extract_refs_and_sources("{{ dispatch() }}", macro_prefix).0;
    assert!(refs.iter().any(|r| r.name == "prefix_transitive_true"));
    assert!(refs.iter().any(|r| r.name == "prefix_transitive_false"));
}

#[test]
fn test_unused_prefix_scalar_macro_is_not_recovered() {
    let macro_prefix = r#"
            {% macro used() %}{{ ref('prefix_used') }}{% endmacro %}
            {% macro unused() %}
                {% if execute %}{{ ref('prefix_unused_true') }}{% else %}{{ ref('prefix_unused_false') }}{% endif %}
            {% endmacro %}
        "#;
    let refs = extract_refs_and_sources("{{ used() }}", macro_prefix).0;
    assert!(refs.iter().any(|r| r.name == "prefix_used"));
    assert!(!refs.iter().any(|r| r.name == "prefix_unused_true"));
    assert!(!refs.iter().any(|r| r.name == "prefix_unused_false"));
}

#[test]
fn test_prefix_recovery_uses_only_reachable_definition_spans() {
    let macro_prefix = r#"
            {% macro used() %}
                {% if execute %}{{ ref('prefix_used_true') }}{% else %}{{ ref('prefix_used_false') }}{% endif %}
            {% endmacro %}
            {% macro unused() %}{{ ref('prefix_unused') }}{{ source('raw', 'prefix_unused') }}{% endmacro %}
        "#;
    let sql = "{{ used() }}";
    let outcome = super::super::jinja::extract_via_jinja(sql, macro_prefix);
    let plan = outcome.macro_reachability.as_ref().unwrap();
    assert_eq!(plan.prefix_scopes, HashSet::from(["used".to_owned()]));
    assert_eq!(plan.prefix_definition_spans.len(), 1);
    let refs =
        super::recovery::extract_refs_regex_spans(macro_prefix, &plan.prefix_definition_spans);
    assert_eq!(
        refs.iter()
            .map(|reference| reference.name.as_str())
            .collect::<Vec<_>>(),
        vec!["prefix_used_true", "prefix_used_false"]
    );
    assert!(
        !refs
            .iter()
            .any(|reference| reference.name == "prefix_unused")
    );
}

#[test]
fn test_duplicate_reachable_prefix_definitions_union_selected_spans() {
    let macro_prefix = r#"
            {% macro used() %}{{ ref('prefix_first') }}{% endmacro %}
            {% macro used() %}{{ ref('prefix_second') }}{% endmacro %}
        "#;
    let plan = super::super::jinja::reachability::macro_reachability_with_prefix(
        "{{ used() }}",
        macro_prefix,
        None,
        None,
    )
    .unwrap();
    assert_eq!(plan.prefix_definition_spans.len(), 2);
    let refs =
        super::recovery::extract_refs_regex_spans(macro_prefix, &plan.prefix_definition_spans);
    assert!(
        refs.iter()
            .any(|reference| reference.name == "prefix_first")
    );
    assert!(
        refs.iter()
            .any(|reference| reference.name == "prefix_second")
    );
}

#[test]
fn test_incomplete_render_uses_conservative_whole_prefix_recovery() {
    let macro_prefix = "{% macro unused_prefix() %}{{ ref('unused_prefix_ref') }}{% endmacro %}";
    let sql = "{{ missing_macro(ref('model_before_failure')) }}";
    let refs = extract_refs_and_sources(sql, macro_prefix).0;
    assert!(refs.iter().any(|r| r.name == "model_before_failure"));
    assert!(refs.iter().any(|r| r.name == "unused_prefix_ref"));
}

#[test]
fn test_model_uncertainty_selects_prefix_macro_branches_and_sources() {
    let macro_prefix = r#"
            {% macro us() %}{{ ref('prefix_us') }}{{ source('raw', 'us') }}{% endmacro %}
            {% macro eu() %}{{ ref('prefix_eu') }}{{ source('raw', 'eu') }}{% endmacro %}
        "#;
    let sql = "{% if execute %}{{ us() }}{% else %}{{ eu() }}{% endif %}";
    let extracted = extract_all(sql, macro_prefix);
    assert!(extracted.refs.iter().any(|r| r.name == "prefix_us"));
    assert!(extracted.refs.iter().any(|r| r.name == "prefix_eu"));
    assert!(
        extracted
            .sources
            .iter()
            .any(|s| s.source_name == "raw" && s.table_name == "us")
    );
    assert!(
        extracted
            .sources
            .iter()
            .any(|s| s.source_name == "raw" && s.table_name == "eu")
    );
}

#[test]
fn test_prefix_uncertainty_follows_transitive_local_macro_references() {
    let macro_prefix = r#"
            {% macro dispatch() %}
                {% if env_var('REGION') == 'us' %}{{ choose_us() }}{% else %}{{ choose_eu() }}{% endif %}
            {% endmacro %}
            {% macro choose_us() %}{{ left() }}{% endmacro %}
            {% macro choose_eu() %}{{ right() }}{% endmacro %}
        "#;
    let sql = r#"
            {% macro left() %}{{ ref('left_transitive') }}{% endmacro %}
            {% macro right() %}{{ ref('right_transitive') }}{% endmacro %}
            {{ dispatch() }}
        "#;
    let refs = extract_refs_and_sources(sql, macro_prefix).0;
    assert!(refs.iter().any(|r| r.name == "left_transitive"));
    assert!(refs.iter().any(|r| r.name == "right_transitive"));
}

#[test]
fn test_unused_prefix_macro_does_not_reach_local_definition() {
    let macro_prefix = r#"
            {% macro choose() %}
                {% if env_var('REGION') %}{{ used() }}{% endif %}
            {% endmacro %}
            {% macro unused_prefix() %}{{ unused() }}{% endmacro %}
        "#;
    let sql = r#"
            {% macro used() %}{{ ref('used_prefix_graph') }}{% endmacro %}
            {% macro unused() %}{{ ref('unused_prefix_graph') }}{% endmacro %}
            {{ choose() }}
        "#;
    let refs = extract_refs_and_sources(sql, macro_prefix).0;
    assert!(refs.iter().any(|r| r.name == "used_prefix_graph"));
    assert!(!refs.iter().any(|r| r.name == "unused_prefix_graph"));
}

#[test]
fn test_called_model_macro_runtime_branches_are_merged() {
    let sql = r#"
            {% macro runtime_branch() %}
                {% if execute %}{{ ref('called_true') }}{% else %}{{ ref('called_false') }}{% endif %}
            {% endmacro %}
            {% set invoke = runtime_branch %}
            {{ invoke() }}
        "#;
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 2);
    assert!(refs.iter().any(|r| r.name == "called_true"));
    assert!(refs.iter().any(|r| r.name == "called_false"));
}

#[test]
fn test_called_macro_scope_recovery_excludes_uncalled_macro_refs() {
    let sql = r#"
            {% macro called_branch() %}
                {% if env_var('REGION') == 'us' %}
                    {{ ref('called_us') }}
                {% else %}
                    {{ ref('called_eu') }}
                {% endif %}
            {% endmacro %}
            {% macro unused_branch() %}
                {{ ref('unused_ref') }}
            {% endmacro %}
            {{ called_branch() }}
        "#;
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 2);
    assert!(refs.iter().any(|r| r.name == "called_us"));
    assert!(refs.iter().any(|r| r.name == "called_eu"));
    assert!(!refs.iter().any(|r| r.name == "unused_ref"));
}

#[test]
fn test_called_target_macro_scope_recovery_excludes_uncalled_refs() {
    let sql = r#"
            {% macro called_branch() %}
                {% if target.name == 'us' %}{{ ref('target_us') }}{% else %}{{ ref('target_eu') }}{% endif %}
            {% endmacro %}
            {% macro unused_branch() %}{{ ref('unused_target') }}{% endmacro %}
            {{ called_branch() }}
        "#;
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 2);
    assert!(refs.iter().any(|r| r.name == "target_us"));
    assert!(refs.iter().any(|r| r.name == "target_eu"));
    assert!(!refs.iter().any(|r| r.name == "unused_target"));
}

#[test]
fn test_called_missing_var_macro_scope_recovery_excludes_uncalled_refs() {
    let sql = r#"
            {% macro called_branch() %}
                {% if var('REGION') == 'us' %}{{ ref('var_us') }}{% else %}{{ ref('var_eu') }}{% endif %}
            {% endmacro %}
            {% macro unused_branch() %}{{ ref('unused_var') }}{% endmacro %}
            {{ called_branch() }}
        "#;
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 2);
    assert!(refs.iter().any(|r| r.name == "var_us"));
    assert!(refs.iter().any(|r| r.name == "var_eu"));
    assert!(!refs.iter().any(|r| r.name == "unused_var"));
}

#[test]
fn test_transitive_macro_scope_recovery_excludes_uncalled_refs() {
    let sql = r#"
            {% macro inner_branch() %}
                {% if env_var('REGION') == 'us' %}{{ ref('inner_us') }}{% else %}{{ ref('inner_eu') }}{% endif %}
            {% endmacro %}
            {% macro outer_branch() %}{{ inner_branch() }}{% endmacro %}
            {% macro unused_branch() %}{{ ref('unused_transitive') }}{% endmacro %}
            {{ outer_branch() }}
        "#;
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 2);
    assert!(refs.iter().any(|r| r.name == "inner_us"));
    assert!(refs.iter().any(|r| r.name == "inner_eu"));
    assert!(!refs.iter().any(|r| r.name == "unused_transitive"));
}

#[test]
fn test_higher_order_macro_scope_recovery_excludes_uncalled_refs() {
    let sql = r#"
            {% macro callback_branch() %}
                {% if env_var('REGION') == 'us' %}{{ ref('callback_us') }}{% else %}{{ ref('callback_eu') }}{% endif %}
            {% endmacro %}
            {% macro invoke(callback) %}{{ callback() }}{% endmacro %}
            {% macro unused_branch() %}{{ ref('unused_higher_order') }}{% endmacro %}
            {{ invoke(callback_branch) }}
        "#;
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 2);
    assert!(refs.iter().any(|r| r.name == "callback_us"));
    assert!(refs.iter().any(|r| r.name == "callback_eu"));
    assert!(!refs.iter().any(|r| r.name == "unused_higher_order"));
}

#[test]
fn test_scalar_runtime_branches_are_merged() {
    for scalar in ["dbt_version", "invocation_id", "run_started_at"] {
        let sql = format!(
            "{{% if {scalar} %}}SELECT * FROM {{{{ ref('{scalar}_true') }}}}{{% else %}}SELECT * FROM {{{{ ref('{scalar}_false') }}}}{{% endif %}}"
        );
        let refs = extract_refs(&sql);
        assert_eq!(refs.len(), 2, "expected both refs for {scalar}");
        assert!(refs.iter().any(|r| r.name == format!("{scalar}_true")));
        assert!(refs.iter().any(|r| r.name == format!("{scalar}_false")));
    }
}

#[test]
fn test_missing_var_branches_are_merged() {
    let sql = r#"
            {% if var('REGION') == 'us' %}
                SELECT * FROM {{ ref('orders_us') }}
            {% else %}
                SELECT * FROM {{ ref('orders_eu') }}
            {% endif %}
        "#;
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 2);
    assert!(refs.iter().any(|r| r.name == "orders_us"));
    assert!(refs.iter().any(|r| r.name == "orders_eu"));
}

#[test]
fn test_runtime_target_branches_are_merged() {
    let sql = r#"
            {% if target.name == 'us' %}
                SELECT * FROM {{ ref('orders_us') }}
            {% else %}
                SELECT * FROM {{ ref('orders_eu') }}
            {% endif %}
        "#;
    let refs = extract_refs(sql);
    assert_eq!(refs.len(), 2);
    assert!(refs.iter().any(|r| r.name == "orders_us"));
    assert!(refs.iter().any(|r| r.name == "orders_eu"));
}

#[test]
fn test_source_in_unknown_macro_args() {
    let sql = "{{ import_cte(source('raw', 'orders')) }}";
    let sources = extract_sources(sql);
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].source_name, "raw");
    assert_eq!(sources[0].table_name, "orders");
}

#[test]
fn test_defined_macro_called_with_namespace() {
    // Even when the macro body is known, dbt allows calling it with the
    // package namespace, which minijinja can't resolve.
    let macro_src =
        "{% macro import_cte(pairs) %}{% for p in pairs %}{{ p[1] }}{% endfor %}{% endmacro %}";
    let sql = "{{ my_package.import_cte([('orders', ref('int_orders'))]) }}";
    let (refs, _) = extract_refs_and_sources(sql, macro_src);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "int_orders");
}

#[test]
fn test_regex_fallback_ref_in_set_statement() {
    let refs = extract_refs_regex("{% set orders = ref('stg_orders') %}");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "stg_orders");
}

#[test]
fn test_regex_fallback_ignores_ref_outside_jinja_blocks() {
    // A bare ref('...') in a SQL comment is not a jinja call and must not
    // create a dependency.
    let sql = r#"
            -- replaced ref('old_model') with a CTE
            {{ unknown_macro() }}
            SELECT 1
        "#;
    let refs = extract_refs_regex(sql);
    assert!(refs.is_empty());
}

#[test]
fn test_regex_fallback_ignores_raw_blocks() {
    let sql = r#"
            {% raw %}{{ ref('not_a_dep') }}{% endraw %}
            {{ unknown_macro(ref('real_dep')) }}
        "#;
    let refs = extract_refs_regex(sql);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "real_dep");
}

#[test]
fn test_regex_fallback_no_partial_identifier_match() {
    // myref(...) must not be mistaken for ref(...)
    let refs = extract_refs_regex("{{ myref('not_a_model') }}");
    assert!(refs.is_empty());
}

#[test]
fn test_regex_fallback_ignores_ref_text_in_string_literal() {
    // ref(...) appearing inside a string literal is text, not a call
    let refs = extract_refs_regex(r#"{{ unknown_macro("ref('not_a_dep')") }}"#);
    assert!(refs.is_empty());
}

#[test]
fn test_regex_fallback_ignores_source_text_in_string_literal() {
    let sources = extract_sources_regex(r#"{% set msg = "source('raw', 'orders')" %}"#);
    assert!(sources.is_empty());
}

#[test]
fn test_regex_fallback_real_ref_next_to_string_literal_ref_text() {
    let refs =
        extract_refs_regex(r#"{{ unknown_macro('label', "ref('fake')", ref('real_model')) }}"#);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "real_model");
}

#[test]
fn test_regex_fallback_handles_escaped_quotes_in_strings() {
    // The escaped quote must not desynchronize string span tracking
    let refs =
        extract_refs_regex(r#"{{ unknown_macro("a \"quoted\" ref('fake')") + ref('real') }}"#);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "real");
}

#[test]
fn test_combined_regex_fallback_scans_refs_and_sources_once_semantically() {
    let sql = r#"
        -- ref('comment_ref') source('raw', 'comment_source')
        {% raw %}{{ ref('raw_ref') }}{% endraw %}
        {{ unknown_macro("ref('string_ref')", ref('real_ref'), source('raw', 'real_source')) }}
    "#;
    let (refs, sources) = extract_refs_and_sources_regex_scoped(sql, None);
    assert_eq!(
        refs.iter()
            .map(|reference| reference.name.as_str())
            .collect::<Vec<_>>(),
        ["real_ref"]
    );
    assert_eq!(
        sources
            .iter()
            .map(|source| (source.source_name.as_str(), source.table_name.as_str()))
            .collect::<Vec<_>>(),
        [("raw", "real_source")]
    );
}

#[test]
fn test_combined_regex_fallback_preserves_duplicate_selected_spans() {
    let source =
        "{% macro selected() %}{{ ref('selected') }}{{ source('raw', 'selected') }}{% endmacro %}";
    let spans = super::super::jinja::source::model_macro_definition_spans(source);
    let (refs, sources) =
        extract_refs_and_sources_regex_spans(source, &[spans[0].clone(), spans[0].clone()]);
    assert_eq!(refs.len(), 2);
    assert_eq!(sources.len(), 2);
}

#[test]
fn test_config_preserved_when_render_fails_later() {
    let sql = r#"
            {{ config(materialized='incremental', tags=['nightly']) }}
            {{ unknown_macro(ref('a')) }}
        "#;
    let ext = extract_all(sql, "");
    assert_eq!(ext.config.materialized.as_deref(), Some("incremental"));
    assert_eq!(ext.config.tags, vec!["nightly"]);
    assert_eq!(ext.refs.len(), 1);
}

// ─── Config extraction tests ───

#[test]
fn test_config_materialized() {
    let sql = "{{ config(materialized='incremental') }}\nSELECT 1";
    let config = extract_config(sql, "");
    assert_eq!(config.materialized.as_deref(), Some("incremental"));
    assert!(config.tags.is_empty());
}

#[test]
fn test_config_materialized_double_quotes() {
    let sql = r#"{{ config(materialized="table") }}"#;
    let config = extract_config(sql, "");
    assert_eq!(config.materialized.as_deref(), Some("table"));
}

#[test]
fn test_config_tags() {
    let sql = "{{ config(tags=['nightly', 'finance']) }}\nSELECT 1";
    let config = extract_config(sql, "");
    assert_eq!(config.tags, vec!["nightly", "finance"]);
}

#[test]
fn test_config_both() {
    let sql = "{{ config(materialized='view', tags=['daily']) }}\nSELECT 1";
    let config = extract_config(sql, "");
    assert_eq!(config.materialized.as_deref(), Some("view"));
    assert_eq!(config.tags, vec!["daily"]);
}

#[test]
fn test_config_whitespace_control() {
    let sql = "{{- config(materialized='ephemeral') -}}\nSELECT 1";
    let config = extract_config(sql, "");
    assert_eq!(config.materialized.as_deref(), Some("ephemeral"));
}

#[test]
fn test_config_multiline() {
    let sql = r#"{{
            config(
                materialized='incremental',
                tags=['nightly', 'warehouse']
            )
        }}
        SELECT 1"#;
    let config = extract_config(sql, "");
    assert_eq!(config.materialized.as_deref(), Some("incremental"));
    assert_eq!(config.tags, vec!["nightly", "warehouse"]);
}

#[test]
fn test_no_config() {
    let sql = "SELECT * FROM {{ ref('orders') }}";
    let config = extract_config(sql, "");
    assert!(config.materialized.is_none());
    assert!(config.tags.is_empty());
}

#[test]
fn test_config_in_comment_ignored() {
    let sql = r#"
            {# {{ config(materialized='table') }} #}
            SELECT 1
        "#;
    let config = extract_config(sql, "");
    assert!(config.materialized.is_none());
}
