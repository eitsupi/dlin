use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use minijinja::value::{Kwargs, Object, Value, from_args};
use minijinja::{Environment, ErrorKind};

use super::super::sql::{RefCall, SourceCall, normalize_version_str};
use super::{JinjaExtraction, JinjaOutcome, merge_extraction};

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
pub(super) fn render_with_incremental(
    sql: &str,
    macro_prefix: &str,
    vars: &HashMap<String, serde_json::Value>,
) -> JinjaOutcome {
    let (
        (mut extraction, full_complete, full_certain, full_model_uncertain, mut full_scopes),
        (
            incremental_extraction,
            incremental_complete,
            incremental_certain,
            incremental_model_uncertain,
            incremental_scopes,
        ),
    ) = render_with_incremental_passes(sql, macro_prefix, vars);
    merge_extraction(&mut extraction, incremental_extraction);
    for scope in incremental_scopes {
        if !full_scopes.contains(&scope) {
            full_scopes.push(scope);
        }
    }

    JinjaOutcome {
        extraction,
        complete: full_complete && incremental_complete,
        semantic_certain: full_certain && incremental_certain,
        model_uncertain: full_model_uncertain || incremental_model_uncertain,
        uncertain_macro_scopes: full_scopes,
    }
}

/// Compile a dbt SQL template once and render it for both values of
/// `is_incremental`, returning each pass's result separately.
type RenderPass = (JinjaExtraction, bool, bool, bool, Vec<String>);

pub(super) fn render_with_incremental_passes(
    sql: &str,
    macro_prefix: &str,
    vars: &HashMap<String, serde_json::Value>,
) -> (RenderPass, RenderPass) {
    let runtime_analysis = runtime_analysis(sql);
    let marker_names = (!runtime_analysis.macro_spans.is_empty())
        .then(|| unique_runtime_macro_markers(sql, macro_prefix));
    let template_source_without_prefix = marker_names.as_ref().map_or_else(
        || sql.to_owned(),
        |(enter_marker, exit_marker)| {
            super::source::inject_macro_runtime_markers(
                sql,
                &runtime_analysis.macro_spans,
                &runtime_analysis.scalar_macro_names,
                enter_marker,
                exit_marker,
            )
        },
    );
    let template_source = if macro_prefix.is_empty() {
        template_source_without_prefix
    } else {
        format!("{}\n{}", macro_prefix, template_source_without_prefix)
    };
    let render_state = Arc::new(RenderState::default());

    let mut env = Environment::new();
    env.set_undefined_behavior(minijinja::UndefinedBehavior::Lenient);

    if let Some((enter_marker, exit_marker)) = marker_names {
        let state = render_state.clone();
        env.add_function(
            enter_marker,
            move |name: String, scalar_uncertain: bool| -> Result<Value, minijinja::Error> {
                state.enter_macro(name, scalar_uncertain);
                Ok(Value::from(""))
            },
        );
        let state = render_state.clone();
        env.add_function(
            exit_marker,
            move |name: String| -> Result<Value, minijinja::Error> {
                state.exit_macro(&name);
                Ok(Value::from(""))
            },
        );
    }

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
    env.add_function("var", {
        let state = render_state.clone();
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
                state.mark_uncertain();
                Ok(Value::from("__dbt_var_unknown__"))
            }
        }
    });

    // env_var() → returns default or empty string
    env.add_function("env_var", {
        let state = render_state.clone();
        move |args: &[Value]| -> Result<Value, minijinja::Error> {
            // The placeholder value below cannot determine which branch
            // dbt will render. Mark uncertainty only when the stub is
            // actually called; merely registering it must not make every
            // template fall back to regex extraction.
            state.mark_uncertain();
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
            state.mark_uncertain();
            Ok(Value::from(""))
        }
    });

    // statement → no-op
    env.add_function("statement", {
        let state = render_state.clone();
        move |_args: &[Value]| -> Result<Value, minijinja::Error> {
            state.mark_uncertain();
            Ok(Value::from(""))
        }
    });

    // Common dbt globals. Runtime objects are wrappers so attribute access is
    // marked uncertain only when the executable template actually evaluates
    // it; SQL text, comments, and raw blocks never touch these values.
    for (name, rendered) in [
        ("adapter", "__dbt_adapter__"),
        ("api", "__dbt_api__"),
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
                (JinjaExtraction::default(), false, false, false, Vec::new()),
                (JinjaExtraction::default(), false, false, false, Vec::new()),
            );
        }
    };

    let render_pass = |is_incremental: bool| {
        render_state
            .is_incremental
            .store(is_incremental, Ordering::Relaxed);
        render_state.macro_scopes.lock().unwrap().clear();
        render_state.uncertain_macro_scopes.lock().unwrap().clear();
        render_state
            .semantic_certain
            .store(!runtime_analysis.uses_runtime_scalar, Ordering::Relaxed);
        render_state
            .model_uncertain
            .store(runtime_analysis.uses_runtime_scalar, Ordering::Relaxed);
        let complete = template.render_captured_to((), std::io::sink()).is_ok();
        let extraction = std::mem::take(&mut *render_state.extraction.lock().unwrap());
        let semantic_certain = render_state.semantic_certain.load(Ordering::Relaxed);
        let model_uncertain = render_state.model_uncertain.load(Ordering::Relaxed);
        let uncertain_scopes = render_state.uncertain_scopes();
        (
            extraction,
            complete,
            semantic_certain,
            model_uncertain,
            uncertain_scopes,
        )
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
    model_uncertain: AtomicBool,
    macro_scopes: Mutex<Vec<String>>,
    uncertain_macro_scopes: Mutex<HashSet<String>>,
}

impl RenderState {
    fn mark_uncertain(&self) {
        self.semantic_certain.store(false, Ordering::Relaxed);
        let scopes = self.macro_scopes.lock().unwrap();
        if scopes.is_empty() {
            self.model_uncertain.store(true, Ordering::Relaxed);
        } else {
            self.uncertain_macro_scopes
                .lock()
                .unwrap()
                .extend(scopes.iter().cloned());
        }
    }

    fn enter_macro(&self, name: String, scalar_uncertain: bool) {
        self.macro_scopes.lock().unwrap().push(name);
        if scalar_uncertain {
            self.mark_uncertain();
        }
    }

    fn exit_macro(&self, name: &str) {
        let mut scopes = self.macro_scopes.lock().unwrap();
        if scopes.last().is_some_and(|active| active == name) {
            scopes.pop();
        }
    }

    fn uncertain_scopes(&self) -> Vec<String> {
        self.uncertain_macro_scopes
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect()
    }
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
        self.state.mark_uncertain();
        None
    }

    fn get_value_by_str(self: &Arc<Self>, _key: &str) -> Option<Value> {
        self.state.mark_uncertain();
        None
    }

    fn is_true(self: &Arc<Self>) -> bool {
        true
    }

    fn render(self: &Arc<Self>, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.rendered)
    }
}

const RUNTIME_SCALAR_NAMES: [&str; 4] =
    ["execute", "dbt_version", "invocation_id", "run_started_at"];

const RUNTIME_SCOPE_HINT_NAMES: [&str; 17] = [
    "execute",
    "dbt_version",
    "invocation_id",
    "run_started_at",
    "env_var",
    "var",
    "run_query",
    "statement",
    "adapter",
    "api",
    "exceptions",
    "graph",
    "model",
    "modules",
    "target",
    "this",
    "flags",
];

#[derive(Debug)]
pub(super) struct RuntimeAnalysis {
    pub(super) uses_runtime_scalar: bool,
    pub(super) macro_spans: Vec<super::source::ModelMacroSpan>,
    pub(super) scalar_macro_names: HashSet<String>,
}

pub(super) fn runtime_analysis(sql: &str) -> RuntimeAnalysis {
    let has_scalar_hint = RUNTIME_SCALAR_NAMES.iter().any(|name| sql.contains(name));
    if !RUNTIME_SCOPE_HINT_NAMES
        .iter()
        .any(|name| sql.contains(name))
    {
        return RuntimeAnalysis {
            uses_runtime_scalar: false,
            macro_spans: Vec::new(),
            scalar_macro_names: HashSet::new(),
        };
    }

    let macro_spans = super::source::model_macro_definition_spans(sql);
    let env = Environment::new();
    let direct_use = if has_scalar_hint {
        let model_source =
            super::source::strip_macro_definitions_for_runtime_analysis(sql, &macro_spans);
        env.template_from_str(&model_source)
            .ok()
            .map(|template| {
                let undeclared = template.undeclared_variables(false);
                RUNTIME_SCALAR_NAMES
                    .iter()
                    .any(|name| undeclared.contains(*name))
            })
            .unwrap_or(false)
    } else {
        false
    };
    let scalar_macro_names = if has_scalar_hint {
        macro_spans
            .iter()
            .filter_map(|definition| {
                let template = env
                    .template_from_str(&sql[definition.start..definition.end])
                    .ok()?;
                let undeclared = template.undeclared_variables(false);
                RUNTIME_SCALAR_NAMES
                    .iter()
                    .any(|name| undeclared.contains(*name))
                    .then_some(definition.name.clone())
            })
            .collect()
    } else {
        HashSet::new()
    };

    RuntimeAnalysis {
        uses_runtime_scalar: direct_use,
        macro_spans,
        scalar_macro_names,
    }
}

fn unique_runtime_macro_markers(sql: &str, macro_prefix: &str) -> (String, String) {
    const ENTER_BASE: &str = "__dlin_runtime_macro_enter";
    const EXIT_BASE: &str = "__dlin_runtime_macro_exit";
    let template_source = format!("{macro_prefix}\n{sql}");
    for suffix in 0.. {
        let enter = if suffix == 0 {
            ENTER_BASE.to_owned()
        } else {
            format!("{ENTER_BASE}_{suffix}")
        };
        let exit = if suffix == 0 {
            EXIT_BASE.to_owned()
        } else {
            format!("{EXIT_BASE}_{suffix}")
        };
        if !template_source.contains(&enter) && !template_source.contains(&exit) {
            return (enter, exit);
        }
    }
    unreachable!("finite source cannot contain every marker suffix")
}
