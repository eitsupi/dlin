use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use minijinja::value::{from_args, Kwargs};
use minijinja::{Environment, ErrorKind, Value};

use super::sql::{RefCall, SourceCall, SqlConfig};

/// All extracted information from rendering a dbt Jinja SQL template
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct JinjaExtraction {
    pub refs: Vec<RefCall>,
    pub sources: Vec<SourceCall>,
    pub config: SqlConfig,
}

/// Try to extract refs, sources, and config from SQL content using minijinja.
/// Renders twice (with `is_incremental()` returning both false and true) and
/// merges results to capture refs/sources from all conditional branches.
/// Returns `None` if the template fails to render (caller should fall back to regex).
///
/// `macro_prefix` is the pre-built concatenation of valid macro SQL files.
/// It is prepended to the template so that custom macros containing
/// ref()/source() calls are expanded and tracked.
pub fn extract_via_jinja(sql: &str, macro_prefix: &str) -> Option<JinjaExtraction> {
    extract_via_jinja_with_vars(sql, macro_prefix, &HashMap::new())
}

/// Like [`extract_via_jinja`] but resolves `var()` calls using the given
/// project-level variables (parsed from `dbt_project.yml`).
pub fn extract_via_jinja_with_vars(
    sql: &str,
    macro_prefix: &str,
    vars: &HashMap<String, serde_json::Value>,
) -> Option<JinjaExtraction> {
    let template = if macro_prefix.is_empty() {
        sql.to_string()
    } else {
        format!("{}\n{}", macro_prefix, sql)
    };

    // Render with is_incremental=false first (full-load path)
    let mut result = render_with_incremental(&template, false, vars)?;

    // Render again with is_incremental=true to capture incremental-only refs
    if let Some(incr) = render_with_incremental(&template, true, vars) {
        merge_extraction(&mut result, incr);
    }

    Some(result)
}

/// Build a macro prefix string from individual macro sources, skipping
/// any that fail to parse as valid minijinja templates. This ensures one
/// bad macro file doesn't disable jinja-based extraction for all models.
pub fn build_macro_prefix(macro_sources: &[String]) -> String {
    if macro_sources.is_empty() {
        return String::new();
    }
    let env = Environment::new();
    let mut prefix = String::new();
    for source in macro_sources {
        // Only include macros that minijinja can parse individually
        if env.template_from_str(source).is_err() {
            continue;
        }
        // Verify the accumulated prefix still parses after adding this macro
        let len = prefix.len();
        prefix.push_str(source);
        prefix.push('\n');
        if env.template_from_str(&prefix).is_err() {
            prefix.truncate(len);
        }
    }
    prefix
}

/// Merge `other` into `base`, adding only deduplicated refs and sources
fn merge_extraction(base: &mut JinjaExtraction, other: JinjaExtraction) {
    for r in other.refs {
        if !base.refs.contains(&r) {
            base.refs.push(r);
        }
    }
    for s in other.sources {
        if !base.sources.contains(&s) {
            base.sources.push(s);
        }
    }
    // config from first render takes precedence
}

/// Convert a `serde_json::Value` to a `minijinja::Value`.
fn json_to_minijinja(v: &serde_json::Value) -> Value {
    Value::from_serialize(v)
}

/// Render a dbt SQL template once with the given `is_incremental` value.
fn render_with_incremental(
    sql: &str,
    is_incremental: bool,
    vars: &HashMap<String, serde_json::Value>,
) -> Option<JinjaExtraction> {
    let extraction = Arc::new(Mutex::new(JinjaExtraction::default()));

    let mut env = Environment::new();
    env.set_undefined_behavior(minijinja::UndefinedBehavior::Lenient);

    // ref('name'), ref('package', 'name'), or ref('name', version=N)
    // kwargs (e.g. version=2) are appended by minijinja as the last element of args.
    // from_args splits positional args from kwargs so we can extract version.
    let ext = extraction.clone();
    env.add_function(
        "ref",
        move |args: &[Value]| -> Result<Value, minijinja::Error> {
            let mut ext = ext.lock().unwrap();
            let (positional, kwargs): (&[Value], Kwargs) = from_args(args).map_err(|e| {
                minijinja::Error::new(ErrorKind::InvalidOperation, e.to_string())
            })?;
            let version: Option<i64> = kwargs.peek::<i64>("version").ok();
            match positional.len() {
                1 => {
                    let name = positional[0].to_string();
                    ext.refs.push(RefCall {
                        package: None,
                        name: name.clone(),
                        version,
                    });
                    Ok(Value::from(format!("__dbt_ref_{}__", name)))
                }
                2 => {
                    let pkg = positional[0].to_string();
                    let name = positional[1].to_string();
                    ext.refs.push(RefCall {
                        package: Some(pkg),
                        name: name.clone(),
                        version,
                    });
                    Ok(Value::from(format!("__dbt_ref_{}__", name)))
                }
                _ => Err(minijinja::Error::new(
                    ErrorKind::TooManyArguments,
                    "ref() takes 1 or 2 positional arguments",
                )),
            }
        },
    );

    // source('source_name', 'table_name')
    let ext = extraction.clone();
    env.add_function(
        "source",
        move |args: &[Value]| -> Result<Value, minijinja::Error> {
            if args.len() >= 2 {
                let source_name = args[0].to_string();
                let table_name = args[1].to_string();
                ext.lock().unwrap().sources.push(SourceCall {
                    source_name: source_name.clone(),
                    table_name: table_name.clone(),
                });
                Ok(Value::from(format!(
                    "__dbt_source_{}_{}__",
                    source_name, table_name
                )))
            } else {
                Err(minijinja::Error::new(
                    ErrorKind::MissingArgument,
                    "source() requires 2 arguments",
                ))
            }
        },
    );

    // config(materialized='...', tags=[...], ...)
    // Unknown kwargs (schema, alias, unique_key, etc.) are silently ignored.
    let ext = extraction.clone();
    env.add_function(
        "config",
        move |kwargs: Kwargs| -> Result<Value, minijinja::Error> {
            let mut ext = ext.lock().unwrap();
            if let Ok(mat) = kwargs.get::<&str>("materialized") {
                ext.config.materialized = Some(mat.to_string());
            }
            if let Ok(tags_val) = kwargs.get::<Value>("tags")
                && let Ok(iter) = tags_val.try_iter()
            {
                ext.config.tags = iter.map(|v| v.to_string()).collect();
            }
            Ok(Value::from(""))
        },
    );

    // is_incremental() → parameterized
    env.add_function(
        "is_incremental",
        move || -> Result<Value, minijinja::Error> { Ok(Value::from(is_incremental)) },
    );

    // this → dummy relation object
    env.add_global("this", Value::from("__dbt_this__"));

    // var() → resolves from dbt_project.yml vars, then default, then truthy sentinel
    let vars_map: HashMap<String, Value> = vars
        .iter()
        .map(|(k, v)| (k.clone(), json_to_minijinja(v)))
        .collect();
    env.add_function(
        "var",
        move |args: &[Value]| -> Result<Value, minijinja::Error> {
            if let Some(key) = args.first()
                && let Some(key_str) = key.as_str()
                && let Some(val) = vars_map.get(key_str)
            {
                return Ok(val.clone());
            }
            // Fall back to default argument (2nd arg) or truthy sentinel
            if args.len() >= 2 {
                Ok(args[1].clone())
            } else {
                Ok(Value::from("__dbt_var_unknown__"))
            }
        },
    );

    // env_var() → returns default or empty string
    env.add_function(
        "env_var",
        |args: &[Value]| -> Result<Value, minijinja::Error> {
            if args.len() >= 2 {
                Ok(args[1].clone())
            } else {
                Ok(Value::from(""))
            }
        },
    );

    // return() → pass through
    env.add_function(
        "return",
        |args: &[Value]| -> Result<Value, minijinja::Error> {
            Ok(args.first().cloned().unwrap_or(Value::from("")))
        },
    );

    // log() → no-op
    env.add_function(
        "log",
        |_args: &[Value]| -> Result<Value, minijinja::Error> { Ok(Value::from("")) },
    );

    // run_query → no-op
    env.add_function(
        "run_query",
        |_args: &[Value]| -> Result<Value, minijinja::Error> { Ok(Value::from("")) },
    );

    // statement → no-op
    env.add_function(
        "statement",
        |_args: &[Value]| -> Result<Value, minijinja::Error> { Ok(Value::from("")) },
    );

    // Common dbt globals
    env.add_global("adapter", Value::from("__dbt_adapter__"));
    env.add_global("exceptions", Value::from("__dbt_exceptions__"));
    env.add_global("api", Value::from("__dbt_api__"));
    env.add_global("graph", Value::from("__dbt_graph__"));
    env.add_global("target", Value::from("__dbt_target__"));
    env.add_global("invocation_id", Value::from("__dbt_invocation_id__"));
    env.add_global("run_started_at", Value::from("2025-01-01T00:00:00Z"));
    env.add_global("flags", Value::from("__dbt_flags__"));
    env.add_global("modules", Value::from("__dbt_modules__"));
    env.add_global("dbt_version", Value::from("1.0.0"));
    env.add_global("model", Value::from("__dbt_model__"));
    env.add_global("execute", Value::from(true));

    let render_result = env.render_str(sql, ());
    drop(env);

    match render_result {
        Ok(_) => {
            let result = Arc::try_unwrap(extraction)
                .expect("single owner")
                .into_inner()
                .unwrap_or_else(|e| e.into_inner());
            Some(result)
        }
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_ref() {
        let sql = "SELECT * FROM {{ ref('stg_orders') }}";
        let ext = extract_via_jinja(sql, "").unwrap();
        assert_eq!(ext.refs.len(), 1);
        assert_eq!(ext.refs[0].name, "stg_orders");
        assert!(ext.refs[0].package.is_none());
    }

    #[test]
    fn test_two_arg_ref() {
        let sql = "SELECT * FROM {{ ref('other_pkg', 'stg_orders') }}";
        let ext = extract_via_jinja(sql, "").unwrap();
        assert_eq!(ext.refs.len(), 1);
        assert_eq!(ext.refs[0].package.as_deref(), Some("other_pkg"));
        assert_eq!(ext.refs[0].name, "stg_orders");
    }

    #[test]
    fn test_source() {
        let sql = "SELECT * FROM {{ source('raw', 'orders') }}";
        let ext = extract_via_jinja(sql, "").unwrap();
        assert_eq!(ext.sources.len(), 1);
        assert_eq!(ext.sources[0].source_name, "raw");
        assert_eq!(ext.sources[0].table_name, "orders");
    }

    #[test]
    fn test_config() {
        let sql = "{{ config(materialized='incremental', tags=['nightly', 'finance']) }}\nSELECT 1";
        let ext = extract_via_jinja(sql, "").unwrap();
        assert_eq!(ext.config.materialized.as_deref(), Some("incremental"));
        assert_eq!(ext.config.tags, vec!["nightly", "finance"]);
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
        let ext = extract_via_jinja(sql, "").unwrap();
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
        let ext = extract_via_jinja(sql, "").unwrap();
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
        let ext = extract_via_jinja(sql, "").unwrap();
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
        let ext = extract_via_jinja(sql, "").unwrap();
        assert_eq!(ext.refs.len(), 1);
        assert_eq!(ext.refs[0].name, "actual_model");
    }

    #[test]
    fn test_whitespace_control() {
        let sql = "SELECT * FROM {{- ref('stg_orders') -}}";
        let ext = extract_via_jinja(sql, "").unwrap();
        assert_eq!(ext.refs.len(), 1);
        assert_eq!(ext.refs[0].name, "stg_orders");
    }

    #[test]
    fn test_var_with_default() {
        let sql = "SELECT * FROM {{ ref('model_' ~ var('suffix', 'default')) }}";
        let ext = extract_via_jinja(sql, "").unwrap();
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
        let ext = extract_via_jinja_with_vars(sql, "", &vars).unwrap();
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
        let ext = extract_via_jinja_with_vars(sql, "", &vars).unwrap();
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
        let ext = extract_via_jinja_with_vars(sql, "", &vars).unwrap();
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
        let ext = extract_via_jinja_with_vars(sql, "", &vars).unwrap();
        assert_eq!(ext.refs.len(), 1);
        assert_eq!(ext.refs[0].name, "model_fallback");
    }

    #[test]
    fn test_for_loop_with_refs() {
        let sql = r#"
            {% for src in ['orders', 'customers'] %}
                SELECT * FROM {{ source('raw', src) }}
                {% if not loop.last %}UNION ALL{% endif %}
            {% endfor %}
        "#;
        let ext = extract_via_jinja(sql, "").unwrap();
        assert_eq!(ext.sources.len(), 2);
        assert_eq!(ext.sources[0].source_name, "raw");
        assert_eq!(ext.sources[0].table_name, "orders");
        assert_eq!(ext.sources[1].source_name, "raw");
        assert_eq!(ext.sources[1].table_name, "customers");
    }

    #[test]
    fn test_config_with_extra_kwargs() {
        let sql = "{{ config(materialized='incremental', schema='analytics', unique_key='id', tags=['nightly']) }}\nSELECT 1";
        let ext = extract_via_jinja(sql, "").unwrap();
        assert_eq!(ext.config.materialized.as_deref(), Some("incremental"));
        assert_eq!(ext.config.tags, vec!["nightly"]);
    }

    #[test]
    fn test_ref_with_version_kwarg() {
        let sql = "SELECT * FROM {{ ref('my_model', version=2) }}";
        let ext = extract_via_jinja(sql, "").unwrap();
        assert_eq!(ext.refs.len(), 1);
        assert_eq!(ext.refs[0].name, "my_model");
        assert_eq!(ext.refs[0].version, Some(2));
        assert!(ext.refs[0].package.is_none());
    }

    #[test]
    fn test_ref_with_version_kwarg_and_package() {
        let sql = "SELECT * FROM {{ ref('mypkg', 'my_model', version=3) }}";
        let ext = extract_via_jinja(sql, "").unwrap();
        assert_eq!(ext.refs.len(), 1);
        assert_eq!(ext.refs[0].package.as_deref(), Some("mypkg"));
        assert_eq!(ext.refs[0].name, "my_model");
        assert_eq!(ext.refs[0].version, Some(3));
    }

    #[test]
    fn test_ref_without_version_has_none() {
        let sql = "SELECT * FROM {{ ref('my_model') }}";
        let ext = extract_via_jinja(sql, "").unwrap();
        assert_eq!(ext.refs[0].version, None);
    }

    #[test]
    fn test_returns_none_on_unsupported_template() {
        // Unknown block tags should cause failure
        let sql = "{% materialization table, default %} SELECT 1 {% endmaterialization %}";
        let result = extract_via_jinja(sql, "");
        assert!(result.is_none());
    }

    #[test]
    fn test_macro_ref_extraction() {
        let macro_src = r#"
            {% macro my_cte() %}
                SELECT * FROM {{ ref('base_model') }}
            {% endmacro %}
        "#;
        let sql = "SELECT * FROM ({{ my_cte() }})";
        let ext = extract_via_jinja(sql, macro_src).unwrap();
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
        let ext = extract_via_jinja(sql, macro_src).unwrap();
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
        let ext = extract_via_jinja(sql, macro_src).unwrap();
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
        let ext = extract_via_jinja(sql, &prefix).unwrap();
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
    fn test_invalid_macro_skipped_refs_still_extracted() {
        let sources = vec![
            // Bad macro that would poison everything if not filtered
            "{% materialization custom %} stuff {% endmaterialization %}".to_string(),
        ];
        let prefix = build_macro_prefix(&sources);
        let sql = "SELECT * FROM {{ ref('orders') }}";
        let ext = extract_via_jinja(sql, &prefix).unwrap();
        assert_eq!(ext.refs.len(), 1);
        assert_eq!(ext.refs[0].name, "orders");
    }
}
