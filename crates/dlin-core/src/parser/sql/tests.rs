use std::path::PathBuf;

use super::recovery::literal_calls;
use super::*;
use crate::parser::jinja::JinjaExtraction;
use crate::parser::jinja::interp::{MacroLibrary, extract_dependencies};

fn extract_with_macros(sql: &str, macros: &str) -> JinjaExtraction {
    let library = MacroLibrary::new(&[(PathBuf::from("macros.sql"), macros.to_string())]);
    let extraction = extract_dependencies(sql, &library, &Default::default());
    JinjaExtraction {
        refs: extraction.refs,
        sources: extraction.sources,
        config: extraction.config,
    }
}

fn extract_all(sql: &str, macros: &str) -> JinjaExtraction {
    extract_with_macros(sql, macros)
}

fn extract_refs_and_sources(sql: &str, macros: &str) -> (Vec<RefCall>, Vec<SourceCall>) {
    let extraction = extract_with_macros(sql, macros);
    (extraction.refs, extraction.sources)
}

fn extract_refs(sql: &str) -> Vec<RefCall> {
    extract_with_macros(sql, "").refs
}

fn extract_sources(sql: &str) -> Vec<SourceCall> {
    extract_with_macros(sql, "").sources
}

fn extract_config(sql: &str, macros: &str) -> SqlConfig {
    extract_with_macros(sql, macros).config
}

fn extract_refs_regex(sql: &str) -> Vec<RefCall> {
    literal_calls(sql).0
}

fn extract_sources_regex(sql: &str) -> Vec<SourceCall> {
    literal_calls(sql).1
}

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
    let (refs, sources) = literal_calls(sql);
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
