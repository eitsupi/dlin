use super::render::{render_with_incremental_passes, runtime_analysis};
use super::source::{inject_macro_runtime_markers, model_macro_definition_spans};
use super::*;
use minijinja::{Environment, Value};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Extract and assert the template rendered to completion.
fn extract_complete(sql: &str, macro_prefix: &str) -> JinjaExtraction {
    let outcome = extract_via_jinja(sql, macro_prefix);
    assert!(outcome.complete, "expected template to render completely");
    outcome.extraction
}

/// Like [`extract_complete`] with project vars.
fn extract_complete_with_vars(
    sql: &str,
    macro_prefix: &str,
    vars: &HashMap<String, serde_json::Value>,
) -> JinjaExtraction {
    let outcome = extract_via_jinja_with_vars(sql, macro_prefix, vars);
    assert!(outcome.complete, "expected template to render completely");
    outcome.extraction
}

#[test]
fn test_simple_ref() {
    let sql = "SELECT * FROM {{ ref('stg_orders') }}";
    let ext = extract_complete(sql, "");
    assert_eq!(ext.refs.len(), 1);
    assert_eq!(ext.refs[0].name, "stg_orders");
    assert!(ext.refs[0].package.is_none());
}

#[test]
fn test_two_arg_ref() {
    let sql = "SELECT * FROM {{ ref('other_pkg', 'stg_orders') }}";
    let ext = extract_complete(sql, "");
    assert_eq!(ext.refs.len(), 1);
    assert_eq!(ext.refs[0].package.as_deref(), Some("other_pkg"));
    assert_eq!(ext.refs[0].name, "stg_orders");
}

#[test]
fn test_source() {
    let sql = "SELECT * FROM {{ source('raw', 'orders') }}";
    let ext = extract_complete(sql, "");
    assert_eq!(ext.sources.len(), 1);
    assert_eq!(ext.sources[0].source_name, "raw");
    assert_eq!(ext.sources[0].table_name, "orders");
}

#[test]
fn test_config() {
    let sql = "{{ config(materialized='incremental', tags=['nightly', 'finance']) }}\nSELECT 1";
    let ext = extract_complete(sql, "");
    assert_eq!(ext.config.materialized.as_deref(), Some("incremental"));
    assert_eq!(ext.config.tags, vec!["nightly", "finance"]);
}

#[test]
fn test_full_load_config_takes_precedence_over_incremental_config() {
    let sql = r#"
            {% if is_incremental() %}
                {{ config(materialized='incremental', tags=['incremental']) }}
            {% else %}
                {{ config(materialized='table', tags=['full']) }}
            {% endif %}
            SELECT 1
        "#;
    let ext = extract_complete(sql, "");
    assert_eq!(ext.config.materialized.as_deref(), Some("table"));
    assert_eq!(ext.config.tags, vec!["full"]);
}

#[test]
fn test_mixed() {
    let sql = r#"
            {{ config(materialized='table') }}
            SELECT
                o.*,
                c.name
            FROM {{ ref('stg_orders') }} o
            JOIN {{ source('raw', 'customers') }} c ON o.customer_id = c.id
        "#;
    let ext = extract_complete(sql, "");
    assert_eq!(ext.refs.len(), 1);
    assert_eq!(ext.sources.len(), 1);
    assert_eq!(ext.config.materialized.as_deref(), Some("table"));
}

#[test]
fn test_ref_inside_set() {
    let sql = r#"
            {% set orders = ref('stg_orders') %}
            SELECT * FROM {{ orders }}
        "#;
    let ext = extract_complete(sql, "");
    assert_eq!(ext.refs.len(), 1);
    assert_eq!(ext.refs[0].name, "stg_orders");
}

#[test]
fn test_is_incremental_both_branches() {
    let sql = r#"
            {% if is_incremental() %}
            SELECT * FROM {{ ref('stg_incremental_orders') }}
            WHERE updated_at > (SELECT max(updated_at) FROM {{ this }})
            {% else %}
            SELECT * FROM {{ ref('stg_full_orders') }}
            {% endif %}
        "#;
    let ext = extract_complete(sql, "");
    // Both branches are rendered: unique refs from each branch
    assert_eq!(ext.refs.len(), 2);
    assert!(ext.refs.iter().any(|r| r.name == "stg_full_orders"));
    assert!(ext.refs.iter().any(|r| r.name == "stg_incremental_orders"));
}

#[test]
fn test_jinja_comment_ignored() {
    let sql = r#"
            {# This is a comment with {{ ref('should_be_ignored') }} #}
            SELECT * FROM {{ ref('actual_model') }}
        "#;
    let ext = extract_complete(sql, "");
    assert_eq!(ext.refs.len(), 1);
    assert_eq!(ext.refs[0].name, "actual_model");
}

#[test]
fn test_whitespace_control() {
    let sql = "SELECT * FROM {{- ref('stg_orders') -}}";
    let ext = extract_complete(sql, "");
    assert_eq!(ext.refs.len(), 1);
    assert_eq!(ext.refs[0].name, "stg_orders");
}

#[test]
fn test_var_with_default() {
    let sql = "SELECT * FROM {{ ref('model_' ~ var('suffix', 'default')) }}";
    let ext = extract_complete(sql, "");
    assert_eq!(ext.refs.len(), 1);
    assert_eq!(ext.refs[0].name, "model_default");
}

#[test]
fn test_var_resolved_from_project_vars() {
    let sql = "SELECT * FROM {{ ref('model_' ~ var('suffix')) }}";
    let mut vars = HashMap::new();
    vars.insert(
        "suffix".to_string(),
        serde_json::Value::String("prod".to_string()),
    );
    let ext = extract_complete_with_vars(sql, "", &vars);
    assert_eq!(ext.refs.len(), 1);
    assert_eq!(ext.refs[0].name, "model_prod");
}

#[test]
fn test_var_list_expansion_in_for_loop() {
    // Reproduces the reported bug: var() returning a list should iterate
    // as a list, not char-by-char as a string.
    let sql = r#"
            {%- set categories = var("product_categories") -%}
            {%- for cat in categories -%}
                SELECT * FROM {{ ref('stg_' ~ cat ~ '_summary') }}
                {% if not loop.last %}UNION ALL{% endif %}
            {% endfor -%}
        "#;
    let mut vars = HashMap::new();
    vars.insert(
        "product_categories".to_string(),
        serde_json::json!(["electronics", "clothing"]),
    );
    let ext = extract_complete_with_vars(sql, "", &vars);
    assert_eq!(ext.refs.len(), 2);
    assert!(ext.refs.iter().any(|r| r.name == "stg_electronics_summary"));
    assert!(ext.refs.iter().any(|r| r.name == "stg_clothing_summary"));
}

#[test]
fn test_var_project_overrides_default() {
    // When project vars are provided, they should take precedence over
    // the default argument in var().
    let sql = "SELECT * FROM {{ ref('model_' ~ var('env', 'dev')) }}";
    let mut vars = HashMap::new();
    vars.insert(
        "env".to_string(),
        serde_json::Value::String("staging".to_string()),
    );
    let ext = extract_complete_with_vars(sql, "", &vars);
    assert_eq!(ext.refs.len(), 1);
    assert_eq!(ext.refs[0].name, "model_staging");
}

#[test]
fn test_var_unknown_falls_back_to_default() {
    // When a var is not in project vars, fall back to the default argument.
    let sql = "SELECT * FROM {{ ref('model_' ~ var('missing', 'fallback')) }}";
    let mut vars = HashMap::new();
    vars.insert(
        "other_var".to_string(),
        serde_json::Value::String("unused".to_string()),
    );
    let ext = extract_complete_with_vars(sql, "", &vars);
    assert_eq!(ext.refs.len(), 1);
    assert_eq!(ext.refs[0].name, "model_fallback");
}

#[test]
fn test_missing_var_without_default_marks_semantic_uncertainty() {
    let sql = r#"
            {% if var('region') == 'us' %}
                {{ ref('orders_us') }}
            {% else %}
                {{ ref('orders_eu') }}
            {% endif %}
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(!outcome.semantic_certain);
    assert!(outcome.model_uncertain);
    assert_eq!(outcome.extraction.refs.len(), 1);
    assert_eq!(outcome.extraction.refs[0].name, "orders_eu");
}

#[test]
fn test_for_loop_with_refs() {
    let sql = r#"
            {% for src in ['orders', 'customers'] %}
                SELECT * FROM {{ source('raw', src) }}
                {% if not loop.last %}UNION ALL{% endif %}
            {% endfor %}
        "#;
    let ext = extract_complete(sql, "");
    assert_eq!(ext.sources.len(), 2);
    assert_eq!(ext.sources[0].source_name, "raw");
    assert_eq!(ext.sources[0].table_name, "orders");
    assert_eq!(ext.sources[1].source_name, "raw");
    assert_eq!(ext.sources[1].table_name, "customers");
}

#[test]
fn test_config_with_extra_kwargs() {
    let sql = "{{ config(materialized='incremental', schema='analytics', unique_key='id', tags=['nightly']) }}\nSELECT 1";
    let ext = extract_complete(sql, "");
    assert_eq!(ext.config.materialized.as_deref(), Some("incremental"));
    assert_eq!(ext.config.tags, vec!["nightly"]);
}

#[test]
fn test_ref_with_version_kwarg() {
    let sql = "SELECT * FROM {{ ref('my_model', version=2) }}";
    let ext = extract_complete(sql, "");
    assert_eq!(ext.refs.len(), 1);
    assert_eq!(ext.refs[0].name, "my_model");
    assert_eq!(ext.refs[0].version.as_deref(), Some("2"));
    assert!(ext.refs[0].package.is_none());
}

#[test]
fn test_ref_with_version_kwarg_and_package() {
    let sql = "SELECT * FROM {{ ref('mypkg', 'my_model', version=3) }}";
    let ext = extract_complete(sql, "");
    assert_eq!(ext.refs.len(), 1);
    assert_eq!(ext.refs[0].package.as_deref(), Some("mypkg"));
    assert_eq!(ext.refs[0].name, "my_model");
    assert_eq!(ext.refs[0].version.as_deref(), Some("3"));
}

#[test]
fn test_ref_without_version_has_none() {
    let sql = "SELECT * FROM {{ ref('my_model') }}";
    let ext = extract_complete(sql, "");
    assert_eq!(ext.refs[0].version, None);
}

#[test]
fn test_ref_with_v_shorthand_kwarg() {
    let sql = "SELECT * FROM {{ ref('my_model', v=2) }}";
    let ext = extract_complete(sql, "");
    assert_eq!(ext.refs.len(), 1);
    assert_eq!(ext.refs[0].name, "my_model");
    assert_eq!(ext.refs[0].version.as_deref(), Some("2"));
    assert!(ext.refs[0].package.is_none());
}

#[test]
fn test_ref_with_v_shorthand_kwarg_and_package() {
    let sql = "SELECT * FROM {{ ref('mypkg', 'my_model', v=3) }}";
    let ext = extract_complete(sql, "");
    assert_eq!(ext.refs.len(), 1);
    assert_eq!(ext.refs[0].package.as_deref(), Some("mypkg"));
    assert_eq!(ext.refs[0].name, "my_model");
    assert_eq!(ext.refs[0].version.as_deref(), Some("3"));
}

#[test]
fn test_ref_with_string_version_kwarg() {
    // version='alpha' (non-numeric string) passes through unchanged
    let sql = "SELECT * FROM {{ ref('my_model', version='alpha') }}";
    let ext = extract_complete(sql, "");
    assert_eq!(ext.refs[0].version.as_deref(), Some("alpha"));
}

#[test]
fn test_ref_with_padded_integer_version_kwarg() {
    // version='02' (string kwarg) must normalize to "2"
    let sql = "SELECT * FROM {{ ref('my_model', version='02') }}";
    let ext = extract_complete(sql, "");
    assert_eq!(ext.refs[0].version.as_deref(), Some("2"));
}

#[test]
fn test_ref_with_decimal_version_kwarg() {
    // version='2.0' stays as "2.0" — matching YAML `v: "2.0"` which also keeps "2.0".
    let sql = "SELECT * FROM {{ ref('my_model', version='2.0') }}";
    let ext = extract_complete(sql, "");
    assert_eq!(ext.refs[0].version.as_deref(), Some("2.0"));
}

#[test]
fn test_incomplete_on_unsupported_template() {
    // Unknown block tags should mark the outcome as incomplete
    let sql = "{% materialization table, default %} SELECT 1 {% endmaterialization %}";
    let result = extract_via_jinja(sql, "");
    assert!(!result.complete);
}

#[test]
fn test_env_var_call_marks_semantic_uncertainty() {
    let sql = r#"
            {% if env_var('REGION') == 'us' %}
                SELECT * FROM {{ ref('orders_us') }}
            {% else %}
                SELECT * FROM {{ ref('orders_eu') }}
            {% endif %}
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(!outcome.semantic_certain);
    assert!(outcome.model_uncertain);
}

#[test]
fn test_runtime_global_attribute_marks_semantic_uncertainty() {
    let sql = r#"
            {% if target.name == 'us' %}
                SELECT * FROM {{ ref('orders_us') }}
            {% else %}
                SELECT * FROM {{ ref('orders_eu') }}
            {% endif %}
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(!outcome.semantic_certain);
    assert!(outcome.model_uncertain);
}

#[test]
fn test_runtime_global_item_access_marks_semantic_uncertainty() {
    let sql = r#"
            {% if target['name'] == 'us' %}SELECT 1{% endif %}
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(!outcome.semantic_certain);
}

#[test]
fn test_runtime_global_attribute_keeps_undefined_truthiness() {
    let sql = r#"
            {% if target.name %}
                {{ ref('target_true') }}
            {% else %}
                {{ ref('target_false') }}
            {% endif %}
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(!outcome.semantic_certain);
    assert_eq!(outcome.extraction.refs.len(), 1);
    assert_eq!(outcome.extraction.refs[0].name, "target_false");
}

#[test]
fn test_runtime_global_truthiness_matches_sentinel() {
    let sql = r#"
            {% if target %}{{ ref('target_true') }}{% else %}{{ ref('target_false') }}{% endif %}
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(outcome.semantic_certain);
    assert_eq!(outcome.extraction.refs.len(), 1);
    assert_eq!(outcome.extraction.refs[0].name, "target_true");
}

#[test]
fn test_execute_keeps_placeholder_branch_but_marks_uncertainty() {
    let sql = r#"
            {% if execute %}
                {{ ref('execute_true') }}
            {% else %}
                {{ ref('execute_false') }}
            {% endif %}
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(!outcome.semantic_certain);
    assert_eq!(outcome.extraction.refs.len(), 1);
    assert_eq!(outcome.extraction.refs[0].name, "execute_true");
}

#[test]
fn test_scalar_runtime_context_ignores_non_executable_text() {
    let sql = r#"
            -- execute dbt_version invocation_id run_started_at
            {# execute dbt_version invocation_id run_started_at #}
            {% raw %}execute dbt_version invocation_id run_started_at{% endraw %}
            {% set label = "execute" %}
            SELECT 1
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(outcome.semantic_certain);
}

#[test]
fn test_runtime_scalar_analysis_fast_path_without_scalar_names() {
    let analysis = runtime_analysis("SELECT 1", "");
    assert!(!analysis.uses_runtime_scalar);
    assert!(analysis.macro_spans.is_empty());
}

#[test]
fn test_runtime_analysis_keeps_prefix_macro_roots_from_model_scan() {
    let sql = "{% if execute %}{{ choose() }}{% endif %}";
    let prefix = "{% macro choose() %}prefix{% endmacro %}";
    let analysis = runtime_analysis(sql, prefix);
    assert_eq!(
        analysis.model_macro_roots,
        Some(HashSet::from(["choose".to_owned()]))
    );
}

#[test]
fn test_top_level_scalar_does_not_compile_or_mark_inert_local_macros() {
    let mut sql = String::new();
    for index in 0..128 {
        sql.push_str(&format!(
            "{{% macro inert_{index}() %}}inert{{% endmacro %}}\n"
        ));
    }
    sql.push_str("{% if execute %}selected{% endif %}");

    let analysis = runtime_analysis(&sql, "");
    assert_eq!(analysis.scalar_macro_compile_count, 0);
    assert!(analysis.marker_macro_spans.is_empty());
}

#[test]
fn test_prepared_prefix_catalog_initializes_once_across_parallel_extractions() {
    let prefix_source = "{% macro choose() %}{% if execute %}{{ left() }}{% else %}{{ right() }}{% endif %}{% endmacro %}\n"
        .to_owned()
        + "{% macro left() %}left{% endmacro %}\n"
        + "{% macro right() %}right{% endmacro %}\n"
        + "{% macro unused() %}{% if execute %}unused{% endif %}{% endmacro %}";
    let prefix = Arc::new(reachability::PreparedMacroPrefix::new(&prefix_source));
    (0..16).into_par_iter().for_each(|_| {
        let _ = extract_via_jinja_with_prepared_prefix("{{ choose() }}", &prefix, &HashMap::new());
    });
    assert_eq!(prefix.catalog_initializations(), 1);
    assert_eq!(prefix.initialized_definition_count(), 3);
}

#[test]
fn test_scalar_runtime_context_ignores_unused_macro_prefix() {
    let macro_prefix = r#"
            {% macro runtime_branch() %}
                {% if execute %}runtime{% endif %}
            {% endmacro %}
        "#;
    let outcome = extract_via_jinja("SELECT 1", macro_prefix);
    assert!(outcome.complete);
    assert!(outcome.semantic_certain);
}

#[test]
fn test_scalar_runtime_context_ignores_uncalled_model_macro() {
    let sql = r#"
            {% macro unused_runtime_branch() %}
                {% if execute %}{{ ref('unused_true') }}{% endif %}
            {% endmacro %}
            SELECT 1
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(outcome.semantic_certain);
    assert!(outcome.extraction.refs.is_empty());
}

#[test]
fn test_scalar_runtime_context_ignores_macro_like_raw_text() {
    let sql = r#"
            {% raw %}{% macro fake() %}{% if execute %}raw{% endif %}{% endmacro %}{% endraw %}
            SELECT 1
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(outcome.semantic_certain);
}

#[test]
fn test_scalar_runtime_context_tracks_called_model_macro() {
    let sql = r#"
            {% macro runtime_branch() %}
                {% if execute %}{{ ref('called_true') }}{% else %}{{ ref('called_false') }}{% endif %}
            {% endmacro %}
            {{ runtime_branch() }}
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(!outcome.semantic_certain);
    assert!(!outcome.model_uncertain);
    assert_eq!(outcome.extraction.refs.len(), 1);
    assert_eq!(outcome.extraction.refs[0].name, "called_true");
}

#[test]
fn test_runtime_macro_marker_preserves_whitespace_controlled_output() {
    let sources = [
        "{% macro text() %}value{% endmacro %}{{ text() }}",
        "{% macro text() -%}\nvalue{% endmacro %}{{ text() }}",
        "{%+ macro text() +%}\nvalue{%+ endmacro +%}{{ text() }}",
    ];
    for source in sources {
        let spans = super::source::model_macro_definition_spans(source);
        assert_eq!(spans.len(), 1);
        let transformed = super::source::inject_macro_runtime_markers(
            source,
            &spans,
            &HashSet::new(),
            "enter",
            "exit",
        );

        let original = Environment::new()
            .template_from_str(source)
            .unwrap()
            .render(())
            .unwrap();
        let mut instrumented_env = Environment::new();
        instrumented_env.add_function(
            "enter",
            |_: String, _: bool| -> Result<Value, minijinja::Error> { Ok(Value::from("")) },
        );
        instrumented_env.add_function("exit", |_: String| -> Result<Value, minijinja::Error> {
            Ok(Value::from(""))
        });
        let instrumented = instrumented_env
            .template_from_str(&transformed)
            .unwrap()
            .render(())
            .unwrap();
        assert_eq!(instrumented, original);
    }
}

#[test]
fn test_runtime_macro_marker_preserves_return_value_in_condition() {
    let sql = r#"
            {% macro choose() -%}
            yes{%- if execute %}{%- endif -%}
            {%- endmacro %}
            {% if choose() == 'yes' %}{{ ref('return_yes') }}{% else %}{{ ref('return_no') }}{% endif %}
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(!outcome.semantic_certain);
    assert_eq!(outcome.extraction.refs.len(), 1);
    assert_eq!(outcome.extraction.refs[0].name, "return_yes");
}

#[test]
fn test_scalar_runtime_context_tracks_whitespace_controlled_macro() {
    let sql = r#"
            {%+ macro runtime_branch() +%}
                {% if execute %}{{ ref('plus_true') }}{% else %}{{ ref('plus_false') }}{% endif %}
            {%+ endmacro +%}
            {{ runtime_branch() }}
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(!outcome.semantic_certain);
    assert_eq!(outcome.extraction.refs.len(), 1);
    assert_eq!(outcome.extraction.refs[0].name, "plus_true");
}

#[test]
fn test_scalar_runtime_context_tracks_alias_chain() {
    let sql = r#"
            {% macro runtime_branch() %}
                {% if execute %}{{ ref('alias_true') }}{% else %}{{ ref('alias_false') }}{% endif %}
            {% endmacro %}
            {% set first = runtime_branch %}
            {% set second = first %}
            {{ second() }}
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(!outcome.semantic_certain);
    assert_eq!(outcome.extraction.refs.len(), 1);
    assert_eq!(outcome.extraction.refs[0].name, "alias_true");
}

#[test]
fn test_scalar_runtime_context_tracks_transitive_macro() {
    let sql = r#"
            {% macro inner_branch() %}
                {% if execute %}{{ ref('inner_true') }}{% else %}{{ ref('inner_false') }}{% endif %}
            {% endmacro %}
            {% macro outer_branch() %}{{ inner_branch() }}{% endmacro %}
            {{ outer_branch() }}
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(!outcome.semantic_certain);
    assert_eq!(outcome.extraction.refs.len(), 1);
    assert_eq!(outcome.extraction.refs[0].name, "inner_true");
}

#[test]
fn test_scalar_runtime_context_tracks_higher_order_macro() {
    let sql = r#"
            {% macro runtime_branch() %}
                {% if execute %}{{ ref('higher_true') }}{% else %}{{ ref('higher_false') }}{% endif %}
            {% endmacro %}
            {% macro invoke(callback) %}{{ callback() }}{% endmacro %}
            {{ invoke(runtime_branch) }}
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(!outcome.semantic_certain);
    assert_eq!(outcome.extraction.refs.len(), 1);
    assert_eq!(outcome.extraction.refs[0].name, "higher_true");
}

#[test]
fn test_scalar_runtime_context_ignores_member_macro_call() {
    let sql = r#"
            {% macro runtime_branch() %}{% if execute %}{{ ref('unused_true') }}{% endif %}{% endmacro %}
            {{ obj.runtime_branch() }}
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.semantic_certain);
}

#[test]
fn test_scalar_runtime_context_ignores_macro_string_literal() {
    let sql = r#"
            {% macro literal_runtime_name() %}{{ "execute" }}{% endmacro %}
            {{ literal_runtime_name() }}
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(outcome.semantic_certain);
}

#[test]
fn test_scalar_runtime_context_ignores_called_macro_shadowing() {
    let sql = r#"
            {% macro local_execute() %}
                {% set execute = false %}
                {% if execute %}{{ ref('shadow_true') }}{% else %}{{ ref('shadow_false') }}{% endif %}
            {% endmacro %}
            {{ local_execute() }}
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(outcome.semantic_certain);
    assert_eq!(outcome.extraction.refs.len(), 1);
    assert_eq!(outcome.extraction.refs[0].name, "shadow_false");
}

#[test]
fn test_scalar_runtime_context_ignores_local_shadowing() {
    let sql = r#"
            {% set execute = false %}
            {% if execute %}{{ ref('execute_true') }}{% else %}{{ ref('execute_false') }}{% endif %}
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(outcome.semantic_certain);
    assert_eq!(outcome.extraction.refs.len(), 1);
    assert_eq!(outcome.extraction.refs[0].name, "execute_false");
}

#[test]
fn test_scalar_runtime_context_ignores_member_names() {
    let sql = r#"
            {% set obj = {'execute': true} %}
            {% if obj.execute %}{{ ref('member_true') }}{% else %}{{ ref('member_false') }}{% endif %}
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(outcome.semantic_certain);
    assert_eq!(outcome.extraction.refs.len(), 1);
    assert_eq!(outcome.extraction.refs[0].name, "member_true");
}

#[test]
fn test_api_global_truthiness_matches_sentinel() {
    let sql = r#"
            {% if api %}{{ ref('api_true') }}{% else %}{{ ref('api_false') }}{% endif %}
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(outcome.semantic_certain);
    assert_eq!(outcome.extraction.refs.len(), 1);
    assert_eq!(outcome.extraction.refs[0].name, "api_true");
}

#[test]
fn test_runtime_certainty_ignores_non_executable_text() {
    let sql = r#"
            -- target.name and env_var('REGION') are SQL text
            {# target.name and env_var('REGION') are comments #}
            {% raw %}{{ target.name }} {{ env_var('REGION') }}{% endraw %}
            {%raw%}{{ target.name }}{%endraw%}
            SELECT 1
        "#;
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(outcome.semantic_certain);
}

#[test]
fn test_runtime_stub_is_only_uncertain_when_called() {
    let sql = "SELECT env_var('REGION') AS region";
    let outcome = extract_via_jinja(sql, "");
    assert!(outcome.complete);
    assert!(outcome.semantic_certain);
}

#[test]
fn test_partial_extraction_salvaged_on_render_failure() {
    // minijinja evaluates call arguments before resolving the callee, so
    // refs passed to an unknown macro are recorded before the failure.
    let sql = "{{ shared_macros.import_cte([('orders', ref('int_orders'))]) }}";
    let outcome = extract_via_jinja(sql, "");
    assert!(!outcome.complete);
    assert_eq!(outcome.extraction.refs.len(), 1);
    assert_eq!(outcome.extraction.refs[0].name, "int_orders");
}

#[test]
fn test_failed_pass_does_not_contaminate_successful_pass() {
    let sql = r#"
            {% if is_incremental() %}
                {{ config(materialized='incremental', tags=['incremental']) }}
                SELECT * FROM {{ ref('incremental_dep') }}
            {% else %}
                {{ config(materialized='table', tags=['full']) }}
                {{ unknown_macro(ref('full_dep')) }}
            {% endif %}
        "#;
    let (
        (full, full_complete, full_certain, full_model_uncertain, _),
        (incremental, incremental_complete, incremental_certain, incremental_model_uncertain, _),
    ) = render_with_incremental_passes(sql, "", &HashMap::new());

    assert!(!full_complete);
    assert!(full_certain);
    assert!(!full_model_uncertain);
    assert_eq!(full.config.materialized.as_deref(), Some("table"));
    assert_eq!(full.config.tags, vec!["full"]);
    assert_eq!(full.refs.len(), 1);
    assert_eq!(full.refs[0].name, "full_dep");

    assert!(incremental_complete);
    assert!(incremental_certain);
    assert!(!incremental_model_uncertain);
    assert_eq!(
        incremental.config.materialized.as_deref(),
        Some("incremental")
    );
    assert_eq!(incremental.config.tags, vec!["incremental"]);
    assert_eq!(incremental.refs.len(), 1);
    assert_eq!(incremental.refs[0].name, "incremental_dep");
}

#[test]
fn test_model_uncertainty_is_isolated_between_incremental_passes() {
    let sql = r#"
            {% if is_incremental() %}
                {% if env_var('REGION') == 'us' %}{{ ref('incremental_us') }}{% endif %}
            {% else %}
                {{ ref('full_dep') }}
            {% endif %}
        "#;
    let (
        (full, full_complete, full_certain, full_model_uncertain, full_scopes),
        (
            incremental,
            incremental_complete,
            incremental_certain,
            incremental_model_uncertain,
            incremental_scopes,
        ),
    ) = render_with_incremental_passes(sql, "", &HashMap::new());

    assert!(full_complete);
    assert!(full_certain);
    assert!(!full_model_uncertain);
    assert!(full_scopes.is_empty());
    assert_eq!(full.refs[0].name, "full_dep");

    assert!(incremental_complete);
    assert!(!incremental_certain);
    assert!(incremental_model_uncertain);
    assert!(incremental_scopes.is_empty());
    assert!(incremental.refs.is_empty());
}

#[test]
fn test_macro_ref_extraction() {
    let macro_src = r#"
            {% macro my_cte() %}
                SELECT * FROM {{ ref('base_model') }}
            {% endmacro %}
        "#;
    let sql = "SELECT * FROM ({{ my_cte() }})";
    let ext = extract_complete(sql, macro_src);
    assert_eq!(ext.refs.len(), 1);
    assert_eq!(ext.refs[0].name, "base_model");
}

#[test]
fn test_macro_source_extraction() {
    let macro_src = r#"
            {% macro raw_data(table) %}
                SELECT * FROM {{ source('raw', table) }}
            {% endmacro %}
        "#;
    let sql = "SELECT * FROM ({{ raw_data('orders') }})";
    let ext = extract_complete(sql, macro_src);
    assert_eq!(ext.sources.len(), 1);
    assert_eq!(ext.sources[0].source_name, "raw");
    assert_eq!(ext.sources[0].table_name, "orders");
}

#[test]
fn test_macro_with_multiple_refs() {
    let macro_src = r#"
            {% macro join_tables(period) %}
                SELECT * FROM {{ ref('deals') }}
                LEFT JOIN {{ ref('providers') }} ON 1=1
                LEFT JOIN {{ source('raw', 'prices') }} ON 1=1
            {% endmacro %}
        "#;
    let sql = "{{ join_tables('day') }}";
    let ext = extract_complete(sql, macro_src);
    assert_eq!(ext.refs.len(), 2);
    assert!(ext.refs.iter().any(|r| r.name == "deals"));
    assert!(ext.refs.iter().any(|r| r.name == "providers"));
    assert_eq!(ext.sources.len(), 1);
    assert_eq!(ext.sources[0].table_name, "prices");
}

#[test]
fn test_multiple_macro_files() {
    let sources = vec![
        r#"
            {% macro get_orders() %}
                SELECT * FROM {{ ref('stg_orders') }}
            {% endmacro %}
            "#
        .to_string(),
        r#"
            {% macro get_customers() %}
                SELECT * FROM {{ ref('stg_customers') }}
            {% endmacro %}
            "#
        .to_string(),
    ];
    let prefix = build_macro_prefix(&sources);
    let sql = "{{ get_orders() }} UNION ALL {{ get_customers() }}";
    let ext = extract_complete(sql, &prefix);
    assert_eq!(ext.refs.len(), 2);
    assert!(ext.refs.iter().any(|r| r.name == "stg_orders"));
    assert!(ext.refs.iter().any(|r| r.name == "stg_customers"));
}

#[test]
fn test_build_macro_prefix_skips_invalid() {
    let sources = vec![
        "{% macro good() %}SELECT 1{% endmacro %}".to_string(),
        // Invalid: unsupported block tag
        "{% materialization custom %} stuff {% endmaterialization %}".to_string(),
        "{% macro also_good() %}SELECT 2{% endmacro %}".to_string(),
        // Invalid: unclosed raw block
        "{% raw %}unclosed raw content".to_string(),
    ];
    let prefix = build_macro_prefix(&sources);
    assert!(prefix.contains("{% macro good() %}"));
    assert!(prefix.contains("{% macro also_good() %}"));
    assert!(!prefix.contains("materialization"));
    assert!(!prefix.contains("{% raw %}"));
}

#[test]
fn test_build_macro_prefix_includes_compatible_macros() {
    let env = Environment::new();

    let macro_a = "{% macro a() %}ok{% endmacro %}".to_string();
    let macro_b = "{% macro b() %}ok{% endmacro %}".to_string();
    assert!(env.template_from_str(&macro_a).is_ok());
    assert!(env.template_from_str(&macro_b).is_ok());

    let sources = vec![macro_a, macro_b];
    let prefix = build_macro_prefix(&sources);
    assert!(prefix.contains("{% macro a() %}"));
    assert!(prefix.contains("{% macro b() %}"));
}

#[test]
fn test_build_macro_prefix_preserves_exact_order_and_newlines() {
    let macro_a = "{% macro a() %}ok{% endmacro %}".to_string();
    let invalid = "{% materialization custom %}invalid{% endmaterialization %}".to_string();
    let macro_b = "{% macro b() %}ok{% endmacro %}".to_string();
    let sources = vec![macro_a.clone(), String::new(), invalid, macro_b.clone()];

    assert_eq!(
        build_macro_prefix(&sources),
        format!("{macro_a}\n\n{macro_b}\n")
    );
}

#[test]
fn test_build_macro_prefix_keeps_reparse_for_duplicate_blocks() {
    // MiniJinja's default `multi_template` grammar rejects duplicate block
    // names in one template, even though each source parses individually.
    // Keep the accumulated validation in build_macro_prefix so this valid
    // individually-but-conflicting source is skipped as before.
    let block_a = "{% block shared %}a{% endblock %}".to_string();
    let block_b = "{% block shared %}b{% endblock %}".to_string();
    let env = Environment::new();

    assert!(env.template_from_str(&block_a).is_ok());
    assert!(env.template_from_str(&block_b).is_ok());
    assert!(
        env.template_from_str(&format!("{block_a}\n{block_b}\n"))
            .is_err()
    );
    assert_eq!(
        build_macro_prefix(&[block_a.clone(), block_b]),
        format!("{block_a}\n")
    );
}

#[test]
fn test_invalid_macro_skipped_refs_still_extracted() {
    let sources = vec![
        // Bad macro that would poison everything if not filtered
        "{% materialization custom %} stuff {% endmaterialization %}".to_string(),
    ];
    let prefix = build_macro_prefix(&sources);
    let sql = "SELECT * FROM {{ ref('orders') }}";
    let ext = extract_complete(sql, &prefix);
    assert_eq!(ext.refs.len(), 1);
    assert_eq!(ext.refs[0].name, "orders");
}

#[test]
fn test_model_macro_spans_ignore_comment_and_raw_text() {
    let sql = r#"
            {# {% macro comment_fake() %}{% endmacro %} #}
            {% raw %}{% macro raw_fake() %}{% endmacro %}{% endraw %}
        "#;
    assert!(model_macro_definition_spans(sql).is_empty());
}

#[test]
fn test_model_macro_spans_ignore_quoted_terminators() {
    let sql = r#"
            {% macro quoted(value="%} endmacro") %}
                {% set text = "%}" %}
            {% endmacro %}
        "#;
    let spans = model_macro_definition_spans(sql);
    assert_eq!(spans.len(), 1);
    let definition = &spans[0];
    let transformed = inject_macro_runtime_markers(sql, &spans, &HashSet::new(), "enter", "exit");
    assert_eq!(
        transformed
            .matches("{{ enter(\"quoted\", false) }}")
            .count(),
        1
    );
    assert!(definition.start < definition.end);
    assert_eq!(
        sql[definition.start..definition.end]
            .matches("endmacro")
            .count(),
        2
    );
}

#[test]
fn test_model_macro_spans_support_plus_and_minus_whitespace_control() {
    let sql = r#"
            {%- macro minus() -%}minus{%- endmacro -%}
            {%+ macro plus() +%}plus{%+ endmacro +%}
        "#;
    assert_eq!(model_macro_definition_spans(sql).len(), 2);
}

#[test]
fn test_model_macro_spans_find_multiple_macros() {
    let sql = "{% macro first() %}one{% endmacro %}{% macro second() %}two{% endmacro %}";
    let spans = model_macro_definition_spans(sql);
    assert_eq!(spans.len(), 2);
    assert!(spans[0].end <= spans[1].start);
}

#[test]
fn test_model_macro_spans_ignore_unclosed_macro() {
    let sql = "{% macro unclosed() %}{% if execute %}value";
    assert!(model_macro_definition_spans(sql).is_empty());
    assert_eq!(
        inject_macro_runtime_markers(sql, &[], &HashSet::new(), "enter", "exit"),
        sql
    );
}
