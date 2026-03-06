use std::sync::{Arc, Mutex};

use minijinja::value::Kwargs;
use minijinja::{Environment, ErrorKind, Value};

use super::sql::{RefCall, SourceCall, SqlConfig};

/// All extracted information from rendering a dbt Jinja SQL template
#[derive(Debug, Default)]
pub struct JinjaExtraction {
    pub refs: Vec<RefCall>,
    pub sources: Vec<SourceCall>,
    pub config: SqlConfig,
}

/// Try to extract refs, sources, and config from SQL content using minijinja.
/// Returns `None` if the template fails to render (caller should fall back to regex).
pub fn extract_via_jinja(sql: &str) -> Option<JinjaExtraction> {
    let extraction = Arc::new(Mutex::new(JinjaExtraction::default()));

    let mut env = Environment::new();
    env.set_undefined_behavior(minijinja::UndefinedBehavior::Lenient);

    // ref('name') or ref('package', 'name')
    let ext = extraction.clone();
    env.add_function("ref", move |args: &[Value]| -> Result<Value, minijinja::Error> {
        let mut ext = ext.lock().unwrap();
        match args.len() {
            1 => {
                let name = args[0].to_string();
                ext.refs.push(RefCall {
                    package: None,
                    name: name.clone(),
                });
                Ok(Value::from(format!("__dbt_ref_{}__", name)))
            }
            2 => {
                let pkg = args[0].to_string();
                let name = args[1].to_string();
                ext.refs.push(RefCall {
                    package: Some(pkg),
                    name: name.clone(),
                });
                Ok(Value::from(format!("__dbt_ref_{}__", name)))
            }
            _ => Err(minijinja::Error::new(
                ErrorKind::TooManyArguments,
                "ref() takes 1 or 2 arguments",
            )),
        }
    });

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
    let ext = extraction.clone();
    env.add_function(
        "config",
        move |kwargs: Kwargs| -> Result<Value, minijinja::Error> {
            let mut ext = ext.lock().unwrap();
            if let Ok(mat) = kwargs.get::<&str>("materialized") {
                ext.config.materialized = Some(mat.to_string());
            }
            if let Ok(tags_val) = kwargs.get::<Value>("tags") {
                if let Ok(iter) = tags_val.try_iter() {
                    ext.config.tags = iter.map(|v| v.to_string()).collect();
                }
            }
            kwargs.assert_all_used()?;
            Ok(Value::from(""))
        },
    );

    // is_incremental() → false (captures the full-load path)
    env.add_function(
        "is_incremental",
        || -> Result<Value, minijinja::Error> { Ok(Value::from(false)) },
    );

    // this → dummy relation object
    env.add_global("this", Value::from("__dbt_this__"));

    // var() → returns default or empty string
    env.add_function(
        "var",
        |args: &[Value]| -> Result<Value, minijinja::Error> {
            if args.len() >= 2 {
                Ok(args[1].clone())
            } else {
                Ok(Value::from(""))
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
                .unwrap();
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
        let ext = extract_via_jinja(sql).unwrap();
        assert_eq!(ext.refs.len(), 1);
        assert_eq!(ext.refs[0].name, "stg_orders");
        assert!(ext.refs[0].package.is_none());
    }

    #[test]
    fn test_two_arg_ref() {
        let sql = "SELECT * FROM {{ ref('other_pkg', 'stg_orders') }}";
        let ext = extract_via_jinja(sql).unwrap();
        assert_eq!(ext.refs.len(), 1);
        assert_eq!(ext.refs[0].package.as_deref(), Some("other_pkg"));
        assert_eq!(ext.refs[0].name, "stg_orders");
    }

    #[test]
    fn test_source() {
        let sql = "SELECT * FROM {{ source('raw', 'orders') }}";
        let ext = extract_via_jinja(sql).unwrap();
        assert_eq!(ext.sources.len(), 1);
        assert_eq!(ext.sources[0].source_name, "raw");
        assert_eq!(ext.sources[0].table_name, "orders");
    }

    #[test]
    fn test_config() {
        let sql = "{{ config(materialized='incremental', tags=['nightly', 'finance']) }}\nSELECT 1";
        let ext = extract_via_jinja(sql).unwrap();
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
        let ext = extract_via_jinja(sql).unwrap();
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
        let ext = extract_via_jinja(sql).unwrap();
        assert_eq!(ext.refs.len(), 1);
        assert_eq!(ext.refs[0].name, "stg_orders");
    }

    #[test]
    fn test_is_incremental_false_branch() {
        let sql = r#"
            SELECT * FROM {{ ref('stg_orders') }}
            {% if is_incremental() %}
            WHERE updated_at > (SELECT max(updated_at) FROM {{ this }})
            {% endif %}
        "#;
        let ext = extract_via_jinja(sql).unwrap();
        assert_eq!(ext.refs.len(), 1);
        assert_eq!(ext.refs[0].name, "stg_orders");
    }

    #[test]
    fn test_jinja_comment_ignored() {
        let sql = r#"
            {# This is a comment with {{ ref('should_be_ignored') }} #}
            SELECT * FROM {{ ref('actual_model') }}
        "#;
        let ext = extract_via_jinja(sql).unwrap();
        assert_eq!(ext.refs.len(), 1);
        assert_eq!(ext.refs[0].name, "actual_model");
    }

    #[test]
    fn test_whitespace_control() {
        let sql = "SELECT * FROM {{- ref('stg_orders') -}}";
        let ext = extract_via_jinja(sql).unwrap();
        assert_eq!(ext.refs.len(), 1);
        assert_eq!(ext.refs[0].name, "stg_orders");
    }

    #[test]
    fn test_var_with_default() {
        let sql = "SELECT * FROM {{ ref('model_' ~ var('suffix', 'default')) }}";
        // The concatenated ref arg will be "model_default" - captured by jinja
        let result = extract_via_jinja(sql);
        assert!(result.is_some());
    }

    #[test]
    fn test_for_loop_with_refs() {
        let sql = r#"
            {% for src in ['orders', 'customers'] %}
                SELECT * FROM {{ source('raw', src) }}
                {% if not loop.last %}UNION ALL{% endif %}
            {% endfor %}
        "#;
        let ext = extract_via_jinja(sql).unwrap();
        assert_eq!(ext.sources.len(), 2);
    }

    #[test]
    fn test_returns_none_on_unsupported_template() {
        // Unknown block tags should cause failure
        let sql = "{% materialization table, default %} SELECT 1 {% endmaterialization %}";
        let result = extract_via_jinja(sql);
        assert!(result.is_none());
    }
}
