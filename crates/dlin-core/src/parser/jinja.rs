use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use minijinja::value::{Kwargs, Object, Value, from_args};
use minijinja::{Environment, ErrorKind};

use super::sql::{RefCall, SourceCall, SqlConfig, normalize_version_str};

/// All extracted information from rendering a dbt Jinja SQL template
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct JinjaExtraction {
    pub refs: Vec<RefCall>,
    pub sources: Vec<SourceCall>,
    pub config: SqlConfig,
}

/// Result of a Jinja-based extraction attempt.
///
/// minijinja evaluates function arguments before resolving the callee, so even
/// when rendering fails (e.g. on an unknown macro) the refs/sources recorded up
/// to the failure point are valid. `complete` tells the caller whether rendering
/// finished, while `semantic_certain` tells it whether placeholder runtime
/// values could have selected the wrong branch. The regex fallback should be
/// merged whenever either property is false.
#[derive(Debug, Clone, Default)]
pub struct JinjaOutcome {
    pub extraction: JinjaExtraction,
    pub complete: bool,
    /// Whether the extraction is semantically certain rather than merely
    /// successfully rendered with the placeholder dbt environment.
    pub(crate) semantic_certain: bool,
}

/// Try to extract refs, sources, and config from SQL content using minijinja.
/// Renders twice (with `is_incremental()` returning both false and true) and
/// merges results to capture refs/sources from all conditional branches.
///
/// `macro_prefix` is the pre-built concatenation of valid macro SQL files.
/// It is prepended to the template so that custom macros containing
/// ref()/source() calls are expanded and tracked.
pub fn extract_via_jinja(sql: &str, macro_prefix: &str) -> JinjaOutcome {
    extract_via_jinja_with_vars(sql, macro_prefix, &HashMap::new())
}

/// Like [`extract_via_jinja`] but resolves `var()` calls using the given
/// project-level variables (parsed from `dbt_project.yml`).
pub fn extract_via_jinja_with_vars(
    sql: &str,
    macro_prefix: &str,
    vars: &HashMap<String, serde_json::Value>,
) -> JinjaOutcome {
    render_with_incremental(sql, macro_prefix, vars)
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
pub(super) fn merge_extraction(base: &mut JinjaExtraction, other: JinjaExtraction) {
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
    // config from the first render takes precedence; fill in fields the
    // first render did not produce (e.g. it failed before reaching config())
    if base.config.materialized.is_none() {
        base.config.materialized = other.config.materialized;
    }
    if base.config.tags.is_empty() {
        base.config.tags = other.config.tags;
    }
}

/// Convert a `serde_json::Value` to a `minijinja::Value`.
fn json_to_minijinja(v: &serde_json::Value) -> Value {
    Value::from_serialize(v)
}

/// Compile a dbt SQL template once and render it for both values of
/// `is_incremental`.
///
/// Returns the extraction together with render-completion and semantic-certainty
/// flags. On failure the extraction still holds everything recorded up to the
/// failure point (minijinja evaluates call arguments before resolving the
/// callee, so e.g. `{{ unknown_macro(ref('a')) }}` records `ref('a')`).
fn render_with_incremental(
    sql: &str,
    macro_prefix: &str,
    vars: &HashMap<String, serde_json::Value>,
) -> JinjaOutcome {
    let (
        (mut extraction, full_complete, full_certain),
        (incremental_extraction, incremental_complete, incremental_certain),
    ) = render_with_incremental_passes(sql, macro_prefix, vars);
    merge_extraction(&mut extraction, incremental_extraction);

    JinjaOutcome {
        extraction,
        complete: full_complete && incremental_complete,
        semantic_certain: full_certain && incremental_certain,
    }
}

/// Compile a dbt SQL template once and render it for both values of
/// `is_incremental`, returning each pass's result separately.
fn render_with_incremental_passes(
    sql: &str,
    macro_prefix: &str,
    vars: &HashMap<String, serde_json::Value>,
) -> ((JinjaExtraction, bool, bool), (JinjaExtraction, bool, bool)) {
    let template_source = if macro_prefix.is_empty() {
        sql.to_string()
    } else {
        format!("{}\n{}", macro_prefix, sql)
    };
    let render_state = Arc::new(RenderState::default());

    let mut env = Environment::new();
    env.set_undefined_behavior(minijinja::UndefinedBehavior::Lenient);

    // ref('name'), ref('package', 'name'), or ref('name', version=N)
    // kwargs (e.g. version=2) are appended by minijinja as the last element of args.
    // from_args splits positional args from kwargs so we can extract version.
    let state = render_state.clone();
    env.add_function(
        "ref",
        move |args: &[Value]| -> Result<Value, minijinja::Error> {
            let mut extraction = state.extraction.lock().unwrap();
            let (positional, kwargs): (&[Value], Kwargs) = from_args(args)
                .map_err(|e| minijinja::Error::new(ErrorKind::InvalidOperation, e.to_string()))?;
            // dbt accepts both `version=N` and `v=N` as shorthand.
            // The value may be an integer (version=2) or a quoted string (version='alpha'),
            // matching dbt-core which uses StringOrInteger for version kwargs.
            let version: Option<String> = kwargs
                .peek::<i64>("version")
                .ok()
                .map(|n| n.to_string())
                .or_else(|| {
                    kwargs
                        .peek::<String>("version")
                        .ok()
                        .map(|s| normalize_version_str(&s))
                })
                .or_else(|| kwargs.peek::<i64>("v").ok().map(|n| n.to_string()))
                .or_else(|| {
                    kwargs
                        .peek::<String>("v")
                        .ok()
                        .map(|s| normalize_version_str(&s))
                });
            match positional.len() {
                1 => {
                    let name = positional[0].to_string();
                    extraction.refs.push(RefCall {
                        package: None,
                        name: name.clone(),
                        version,
                    });
                    Ok(Value::from(format!("__dbt_ref_{}__", name)))
                }
                2 => {
                    let pkg = positional[0].to_string();
                    let name = positional[1].to_string();
                    extraction.refs.push(RefCall {
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
    let state = render_state.clone();
    env.add_function(
        "source",
        move |args: &[Value]| -> Result<Value, minijinja::Error> {
            if args.len() >= 2 {
                let source_name = args[0].to_string();
                let table_name = args[1].to_string();
                state.extraction.lock().unwrap().sources.push(SourceCall {
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
    let state = render_state.clone();
    env.add_function(
        "config",
        move |kwargs: Kwargs| -> Result<Value, minijinja::Error> {
            let mut extraction = state.extraction.lock().unwrap();
            if let Ok(mat) = kwargs.get::<&str>("materialized") {
                extraction.config.materialized = Some(mat.to_string());
            }
            if let Ok(tags_val) = kwargs.get::<Value>("tags")
                && let Ok(iter) = tags_val.try_iter()
            {
                extraction.config.tags = iter.map(|v| v.to_string()).collect();
            }
            Ok(Value::from(""))
        },
    );

    // is_incremental() → parameterized
    let state = render_state.clone();
    env.add_function(
        "is_incremental",
        move || -> Result<Value, minijinja::Error> {
            Ok(Value::from(state.is_incremental.load(Ordering::Relaxed)))
        },
    );

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
    env.add_function("env_var", {
        let state = render_state.clone();
        move |args: &[Value]| -> Result<Value, minijinja::Error> {
            // The placeholder value below cannot determine which branch
            // dbt will render. Mark uncertainty only when the stub is
            // actually called; merely registering it must not make every
            // template fall back to regex extraction.
            state.semantic_certain.store(false, Ordering::Relaxed);
            if args.len() >= 2 {
                Ok(args[1].clone())
            } else {
                Ok(Value::from(""))
            }
        }
    });

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
    env.add_function("run_query", {
        let state = render_state.clone();
        move |_args: &[Value]| -> Result<Value, minijinja::Error> {
            state.semantic_certain.store(false, Ordering::Relaxed);
            Ok(Value::from(""))
        }
    });

    // statement → no-op
    env.add_function("statement", {
        let state = render_state.clone();
        move |_args: &[Value]| -> Result<Value, minijinja::Error> {
            state.semantic_certain.store(false, Ordering::Relaxed);
            Ok(Value::from(""))
        }
    });

    // Common dbt globals. Runtime objects are wrappers so attribute access is
    // marked uncertain only when the executable template actually evaluates
    // it; SQL text, comments, and raw blocks never touch these values.
    for (name, rendered) in [
        ("adapter", "__dbt_adapter__"),
        ("exceptions", "__dbt_exceptions__"),
        ("graph", "__dbt_graph__"),
        ("model", "__dbt_model__"),
        ("modules", "__dbt_modules__"),
        ("target", "__dbt_target__"),
        ("this", "__dbt_this__"),
        ("flags", "__dbt_flags__"),
    ] {
        env.add_global(
            name,
            Value::from_object(RuntimeGlobal::new(render_state.clone(), rendered)),
        );
    }
    env.add_global("invocation_id", Value::from("__dbt_invocation_id__"));
    env.add_global("run_started_at", Value::from("2025-01-01T00:00:00Z"));
    env.add_global("dbt_version", Value::from("1.0.0"));
    env.add_global("execute", Value::from(true));

    let template = match env.template_from_str(&template_source) {
        Ok(template) => template,
        Err(_) => {
            return (
                (JinjaExtraction::default(), false, false),
                (JinjaExtraction::default(), false, false),
            );
        }
    };

    let render_pass = |is_incremental: bool| {
        render_state
            .is_incremental
            .store(is_incremental, Ordering::Relaxed);
        render_state.semantic_certain.store(true, Ordering::Relaxed);
        let complete = template.render_captured_to((), std::io::sink()).is_ok();
        let extraction = std::mem::take(&mut *render_state.extraction.lock().unwrap());
        let semantic_certain = render_state.semantic_certain.load(Ordering::Relaxed);
        (extraction, complete, semantic_certain)
    };

    // Compile once, then render each branch independently. Taking the
    // extraction after every pass prevents partial state from a failed render
    // from leaking into the next pass.
    let full_pass = render_pass(false);
    let incremental_pass = render_pass(true);

    (full_pass, incremental_pass)
}

#[derive(Debug, Default)]
struct RenderState {
    is_incremental: AtomicBool,
    extraction: Mutex<JinjaExtraction>,
    semantic_certain: AtomicBool,
}

#[derive(Debug)]
struct RuntimeGlobal {
    state: Arc<RenderState>,
    rendered: &'static str,
}

impl RuntimeGlobal {
    fn new(state: Arc<RenderState>, rendered: &'static str) -> Self {
        Self { state, rendered }
    }
}

impl Object for RuntimeGlobal {
    fn get_value(self: &Arc<Self>, _key: &Value) -> Option<Value> {
        self.state.semantic_certain.store(false, Ordering::Relaxed);
        None
    }

    fn get_value_by_str(self: &Arc<Self>, _key: &str) -> Option<Value> {
        self.state.semantic_certain.store(false, Ordering::Relaxed);
        None
    }

    fn is_true(self: &Arc<Self>) -> bool {
        true
    }

    fn render(self: &Arc<Self>, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.rendered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            (full, full_complete, full_certain),
            (incremental, incremental_complete, incremental_certain),
        ) = render_with_incremental_passes(sql, "", &HashMap::new());

        assert!(!full_complete);
        assert!(full_certain);
        assert_eq!(full.config.materialized.as_deref(), Some("table"));
        assert_eq!(full.config.tags, vec!["full"]);
        assert_eq!(full.refs.len(), 1);
        assert_eq!(full.refs[0].name, "full_dep");

        assert!(incremental_complete);
        assert!(incremental_certain);
        assert_eq!(
            incremental.config.materialized.as_deref(),
            Some("incremental")
        );
        assert_eq!(incremental.config.tags, vec!["incremental"]);
        assert_eq!(incremental.refs.len(), 1);
        assert_eq!(incremental.refs[0].name, "incremental_dep");
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
}
