use anyhow::{Context, Result};
use minijinja::{Environment, ErrorKind, Value};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::input::normalize_path;

use crate::error::DbtLineageError;

/// The project-level portion of dbt's optional `vars.yml` file.
///
/// `vars.yml` may contain other top-level keys in newer dbt releases, but
/// dlin intentionally consumes only this top-level mapping. CLI vars are not
/// part of dlin's input surface yet and can be layered here later without
/// changing the file/project source selection.
#[derive(Debug, Default, Deserialize)]
struct VarsFile {
    #[serde(default)]
    vars: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct DbtProject {
    pub name: String,

    #[serde(rename = "model-paths", default = "default_model_paths")]
    pub model_paths: Vec<String>,

    #[serde(rename = "seed-paths", default = "default_seed_paths")]
    pub seed_paths: Vec<String>,

    #[serde(rename = "snapshot-paths", default = "default_snapshot_paths")]
    pub snapshot_paths: Vec<String>,

    #[serde(rename = "test-paths", default = "default_test_paths")]
    pub test_paths: Vec<String>,

    #[serde(rename = "macro-paths", default = "default_macro_paths")]
    pub macro_paths: Vec<String>,

    #[serde(rename = "analysis-paths", default = "default_analysis_paths")]
    pub analysis_paths: Vec<String>,

    #[serde(default)]
    pub vars: HashMap<String, serde_json::Value>,

    #[serde(default)]
    pub flags: ProjectFlags,
}

#[derive(Debug, Default, Deserialize)]
pub struct ProjectFlags {
    #[serde(default)]
    pub allow_jinja_file_extensions: bool,
}

fn default_model_paths() -> Vec<String> {
    vec!["models".to_string()]
}

fn default_seed_paths() -> Vec<String> {
    vec!["seeds".to_string()]
}

fn default_snapshot_paths() -> Vec<String> {
    vec!["snapshots".to_string()]
}

fn default_test_paths() -> Vec<String> {
    vec!["tests".to_string()]
}

fn default_macro_paths() -> Vec<String> {
    vec!["macros".to_string()]
}

fn default_analysis_paths() -> Vec<String> {
    vec!["analyses".to_string()]
}

// dbt recognizes the project-root vars.yml filename exactly; vars.yaml is not
// an alias, so dlin intentionally follows that behavior.
const DBT_VARS_FILE_NAME: &str = "vars.yml";

/// Return whether a path is a dbt SQL resource filename.
///
/// dbt's `allow_jinja_file_extensions` flag enables only the documented
/// `.sql.j2`, `.sql.jinja2`, and `.sql.jinja` suffixes in addition to `.sql`.
/// Matching is intentionally case-sensitive and exact.
pub fn is_sql_file(path: &Path, allow_jinja_file_extensions: bool) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.ends_with(".sql")
        || (allow_jinja_file_extensions
            && [".sql.j2", ".sql.jinja2", ".sql.jinja"]
                .iter()
                .any(|suffix| name.ends_with(suffix)))
}

/// Return the logical dbt resource name for a recognized SQL filename.
pub fn sql_file_stem(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown");
    [".sql.j2", ".sql.jinja2", ".sql.jinja", ".sql"]
        .iter()
        .find_map(|suffix| name.strip_suffix(suffix))
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
        })
        .to_string()
}

impl DbtProject {
    pub fn load(project_dir: &Path) -> Result<Self> {
        let project_file = project_dir.join("dbt_project.yml");
        if !project_file.exists() {
            return Err(DbtLineageError::ProjectNotFound(project_dir.to_path_buf()).into());
        }

        let content =
            std::fs::read_to_string(&project_file).map_err(|e| DbtLineageError::FileReadError {
                path: project_file.clone(),
                source: e,
            })?;

        let mut raw_project: serde_json::Value =
            super::yaml_from_str(&content, &project_file.display().to_string())
                .context(format!("Failed to parse {}", project_file.display()))?;

        let project_vars = raw_project
            .get("vars")
            .and_then(serde_json::Value::as_object)
            .filter(|vars| !vars.is_empty())
            .map(|vars| {
                vars.iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();

        let vars_file = project_dir.join(DBT_VARS_FILE_NAME);
        let file_vars = if vars_file.exists() {
            let vars_content = std::fs::read_to_string(&vars_file).map_err(|e| {
                DbtLineageError::FileReadError {
                    path: vars_file.clone(),
                    source: e,
                }
            })?;
            let vars_file_data: VarsFile =
                super::yaml_from_str(&vars_content, &vars_file.display().to_string())
                    .context(format!("Failed to parse {}", vars_file.display()))?;
            vars_file_data.vars.filter(|vars| !vars.is_empty())
        } else {
            None
        };

        if file_vars.is_some() && !project_vars.is_empty() {
            return Err(DbtLineageError::ProjectVarsConflict.into());
        }

        // Match dbt's project loading order: only vars.yml values are available
        // while rendering dbt_project.yml. Project-local vars become the SQL
        // rendering source only when vars.yml is absent or empty.
        let empty_vars = HashMap::new();
        let render_vars = file_vars.as_ref().unwrap_or(&empty_vars);
        render_project_fields(&mut raw_project, render_vars, &project_file)?;

        let mut project: DbtProject = serde_json::from_value(raw_project)
            .context(format!("Failed to parse {}", project_file.display()))?;
        if let Some(vars) = file_vars {
            project.vars = vars;
        }
        Ok(project)
    }

    pub fn resolve_paths(&self, project_dir: &Path) -> ResolvedPaths {
        let resolve = |paths: &[String]| -> Vec<PathBuf> {
            paths
                .iter()
                .map(|p| normalize_path(&project_dir.join(p)))
                .collect()
        };
        ResolvedPaths {
            model_paths: resolve(&self.model_paths),
            seed_paths: resolve(&self.seed_paths),
            snapshot_paths: resolve(&self.snapshot_paths),
            test_paths: resolve(&self.test_paths),
            macro_paths: resolve(&self.macro_paths),
            analysis_paths: resolve(&self.analysis_paths),
            allow_jinja_file_extensions: self.flags.allow_jinja_file_extensions,
        }
    }
}

const PROJECT_RENDER_FIELDS: [(&str, &str); 7] = [
    ("name", "name"),
    ("model-paths", "model-paths"),
    ("seed-paths", "seed-paths"),
    ("snapshot-paths", "snapshot-paths"),
    ("test-paths", "test-paths"),
    ("macro-paths", "macro-paths"),
    ("analysis-paths", "analysis-paths"),
];

fn render_project_fields(
    project: &mut serde_json::Value,
    vars: &HashMap<String, serde_json::Value>,
    project_file: &Path,
) -> Result<()> {
    let Some(project_map) = project.as_object_mut() else {
        return Ok(());
    };
    let environment = project_render_environment(vars);

    for (key, field) in PROJECT_RENDER_FIELDS {
        if key == "name" {
            if let Some(value) = project_map.get(key).and_then(|value| value.as_str()) {
                let rendered = render_project_value(&environment, value, field, project_file)?;
                project_map.insert(key.to_owned(), serde_json::Value::String(rendered));
            }
            continue;
        }
        let Some(value) = project_map.get_mut(key) else {
            continue;
        };
        let Some(entries) = value.as_array_mut() else {
            continue;
        };
        for (index, entry) in entries.iter_mut().enumerate() {
            let Some(entry_value) = entry.as_str() else {
                continue;
            };
            *entry = serde_json::Value::String(render_project_value(
                &environment,
                entry_value,
                &format!("{key}[{index}]"),
                project_file,
            )?);
        }
    }

    // This flag is consumed by dlin's file discovery, so render it before the
    // typed project parse as well. Unlike path fields, it must retain its
    // native boolean type after evaluating a vars.yml expression.
    if let Some(flags) = project_map
        .get_mut("flags")
        .and_then(|value| value.as_object_mut())
        && let Some(value) = flags
            .get("allow_jinja_file_extensions")
            .and_then(|value| value.as_str())
    {
        let rendered = render_project_native_value(
            &environment,
            value,
            "flags.allow_jinja_file_extensions",
            project_file,
        )?;
        flags.insert("allow_jinja_file_extensions".to_owned(), rendered);
    }
    Ok(())
}

fn project_render_environment(vars: &HashMap<String, serde_json::Value>) -> Environment<'static> {
    let values = vars.clone();
    let mut environment = Environment::new();
    environment.add_function(
        "var",
        move |args: &[Value]| -> Result<Value, minijinja::Error> {
            let name = args.first().ok_or_else(|| {
                minijinja::Error::new(ErrorKind::MissingArgument, "var() requires a variable name")
            })?;
            let name = name.to_string();
            if let Some(value) = values.get(&name) {
                return Ok(Value::from_serialize(value));
            }
            if let Some(default) = args.get(1) {
                return Ok(default.clone());
            }
            Err(minijinja::Error::new(
                ErrorKind::UndefinedError,
                format!("required variable '{name}' is not defined"),
            ))
        },
    );
    environment
}

fn render_project_value(
    environment: &Environment,
    template: &str,
    field: &str,
    project_file: &Path,
) -> Result<String> {
    let template = environment.template_from_str(template).map_err(|error| {
        DbtLineageError::ProjectFieldRenderError {
            path: project_file.to_path_buf(),
            field: field.to_string(),
            message: error.to_string(),
        }
    })?;
    Ok(template
        .render(())
        .map_err(|error| DbtLineageError::ProjectFieldRenderError {
            path: project_file.to_path_buf(),
            field: field.to_string(),
            message: error.to_string(),
        })?)
}

/// Render a project value while preserving native Jinja expression results.
///
/// Project path fields are intentionally rendered as strings, but boolean
/// project flags must remain booleans for typed deserialization. For a value
/// consisting of one `{{ expression }}`, evaluate the expression directly;
/// mixed text continues to use the string renderer.
fn render_project_native_value(
    environment: &Environment,
    template: &str,
    field: &str,
    project_file: &Path,
) -> Result<serde_json::Value> {
    let trimmed = template.trim();
    if trimmed.starts_with("{{") && trimmed.ends_with("}}") {
        let expression = trimmed[2..trimmed.len() - 2].trim();
        if !expression.is_empty() {
            let value = environment
                .compile_expression(expression)
                .and_then(|expression| expression.eval(()))
                .map_err(|error| DbtLineageError::ProjectFieldRenderError {
                    path: project_file.to_path_buf(),
                    field: field.to_string(),
                    message: error.to_string(),
                })?;
            return serde_json::to_value(value).map_err(|error| {
                DbtLineageError::ProjectFieldRenderError {
                    path: project_file.to_path_buf(),
                    field: field.to_string(),
                    message: error.to_string(),
                }
                .into()
            });
        }
    }
    Ok(serde_json::Value::String(render_project_value(
        environment,
        template,
        field,
        project_file,
    )?))
}

#[derive(Debug)]
pub struct ResolvedPaths {
    pub model_paths: Vec<PathBuf>,
    pub seed_paths: Vec<PathBuf>,
    pub snapshot_paths: Vec<PathBuf>,
    pub test_paths: Vec<PathBuf>,
    pub macro_paths: Vec<PathBuf>,
    pub analysis_paths: Vec<PathBuf>,
    /// Whether dbt-style Jinja-suffixed SQL filenames are enabled.
    pub allow_jinja_file_extensions: bool,
}

impl ResolvedPaths {
    pub fn default_for(project_dir: &Path) -> Self {
        let resolve = |dirs: &[&str]| -> Vec<PathBuf> {
            dirs.iter()
                .map(|d| normalize_path(&project_dir.join(d)))
                .collect()
        };
        ResolvedPaths {
            model_paths: resolve(&["models"]),
            seed_paths: resolve(&["seeds"]),
            snapshot_paths: resolve(&["snapshots"]),
            test_paths: resolve(&["tests"]),
            macro_paths: resolve(&["macros"]),
            analysis_paths: resolve(&["analyses"]),
            allow_jinja_file_extensions: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_defaults() {
        let yaml = "name: my_project\n";
        let project: DbtProject = serde_saphyr::from_str(yaml).unwrap();
        assert_eq!(project.name, "my_project");
        assert_eq!(project.model_paths, vec!["models"]);
        assert_eq!(project.seed_paths, vec!["seeds"]);
        assert_eq!(project.snapshot_paths, vec!["snapshots"]);
        assert_eq!(project.test_paths, vec!["tests"]);
        assert_eq!(project.macro_paths, vec!["macros"]);
        assert_eq!(project.analysis_paths, vec!["analyses"]);
        assert!(!project.flags.allow_jinja_file_extensions);
    }

    #[test]
    fn test_allow_jinja_file_extensions_flag() {
        let yaml = "name: my_project\nflags:\n  allow_jinja_file_extensions: true\n";
        let project: DbtProject = serde_saphyr::from_str(yaml).unwrap();
        assert!(project.flags.allow_jinja_file_extensions);
        assert!(
            project
                .resolve_paths(Path::new("/project"))
                .allow_jinja_file_extensions
        );
    }

    #[test]
    fn test_load_renders_allow_jinja_file_extensions_from_vars_yml() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("dbt_project.yml"),
            "name: test_project\nflags:\n  allow_jinja_file_extensions: \"{{ var('use_jinja_ext') }}\"\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("vars.yml"),
            "vars:\n  use_jinja_ext: true\n",
        )
        .unwrap();

        let project = DbtProject::load(tmp.path()).unwrap();
        assert!(project.flags.allow_jinja_file_extensions);
        assert!(
            project
                .resolve_paths(tmp.path())
                .allow_jinja_file_extensions
        );
    }

    #[test]
    fn test_load_missing_flag_var_has_field_context() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("dbt_project.yml"),
            "name: test_project\nflags:\n  allow_jinja_file_extensions: \"{{ var('missing_flag') }}\"\n",
        )
        .unwrap();

        let error = DbtProject::load(tmp.path()).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<DbtLineageError>(),
            Some(DbtLineageError::ProjectFieldRenderError { field, .. })
                if field == "flags.allow_jinja_file_extensions"
        ));
    }

    #[test]
    fn test_sql_filename_allowlist_and_logical_stem() {
        let accepted = [
            "orders.sql",
            "orders.sql.j2",
            "orders.sql.jinja2",
            "orders.sql.jinja",
        ];
        for filename in accepted {
            let path = Path::new(filename);
            assert!(is_sql_file(path, true), "{filename}");
            assert_eq!(sql_file_stem(path), "orders");
        }
        for filename in [
            "orders.j2",
            "orders.md.jinja",
            "orders.sql.other",
            "orders.sql.jinja.j2",
        ] {
            assert!(!is_sql_file(Path::new(filename), true), "{filename}");
        }
        assert!(!is_sql_file(Path::new("orders.sql.jinja"), false));
    }

    #[test]
    fn test_custom_paths() {
        let yaml = r#"
name: my_project
model-paths: ["models", "extra_models"]
seed-paths: ["data"]
macro-paths: ["macros", "custom_macros"]
"#;
        let project: DbtProject = serde_saphyr::from_str(yaml).unwrap();
        assert_eq!(project.model_paths, vec!["models", "extra_models"]);
        assert_eq!(project.seed_paths, vec!["data"]);
        assert_eq!(project.snapshot_paths, vec!["snapshots"]); // default
        assert_eq!(project.macro_paths, vec!["macros", "custom_macros"]);
        assert_eq!(project.analysis_paths, vec!["analyses"]); // default
    }

    #[test]
    fn test_custom_analysis_paths() {
        let yaml = r#"
name: my_project
analysis-paths: ["analyses", "custom_analyses"]
"#;
        let project: DbtProject = serde_saphyr::from_str(yaml).unwrap();
        assert_eq!(project.analysis_paths, vec!["analyses", "custom_analyses"]);
    }

    #[test]
    fn test_load_from_file() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("dbt_project.yml"), "name: test_project\n").unwrap();

        let project = DbtProject::load(tmp.path()).unwrap();
        assert_eq!(project.name, "test_project");
        assert_eq!(project.model_paths, vec!["models"]);
    }

    #[test]
    fn test_load_vars_yml() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("dbt_project.yml"), "name: test_project\n").unwrap();
        fs::write(
            tmp.path().join("vars.yml"),
            r#"
vars:
  environment: staging
  dimensions: [region, channel]
  settings:
    retries: 3
    enabled: true
other_top_level_key: ignored
"#,
        )
        .unwrap();

        let project = DbtProject::load(tmp.path()).unwrap();
        assert_eq!(project.vars["environment"], serde_json::json!("staging"));
        assert_eq!(
            project.vars["dimensions"],
            serde_json::json!(["region", "channel"])
        );
        assert_eq!(
            project.vars["settings"],
            serde_json::json!({"retries": 3, "enabled": true})
        );
        assert_eq!(project.vars.len(), 3);
    }

    #[test]
    fn test_load_ignores_vars_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("dbt_project.yml"),
            "name: test_project\nvars:\n  source: project\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("vars.yaml"),
            "vars:\n  source: yaml\n  extra: ignored\n",
        )
        .unwrap();

        let project = DbtProject::load(tmp.path()).unwrap();
        assert_eq!(
            project.vars,
            HashMap::from([(
                "source".to_string(),
                serde_json::Value::String("project".to_string())
            )])
        );
    }

    #[test]
    fn test_load_renders_project_name_and_all_path_fields_from_vars_yml() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("dbt_project.yml"),
            r#"
name: "{{ var('project_name') }}"
model-paths: ["{{ var('model_dir') }}"]
seed-paths: ["{{ var('seed_dir') }}"]
snapshot-paths: ["{{ var('snapshot_dir') }}"]
test-paths: ["{{ var('test_dir') }}"]
macro-paths: ["{{ var('macro_dir') }}"]
analysis-paths: ["{{ var('analysis_dir') }}"]
"#,
        )
        .unwrap();
        fs::write(
            tmp.path().join("vars.yml"),
            r#"
vars:
  project_name: rendered_project
  model_dir: custom_models
  seed_dir: custom_seeds
  snapshot_dir: custom_snapshots
  test_dir: custom_tests
  macro_dir: custom_macros
  analysis_dir: custom_analyses
"#,
        )
        .unwrap();

        let project = DbtProject::load(tmp.path()).unwrap();
        assert_eq!(project.name, "rendered_project");
        assert_eq!(project.model_paths, vec!["custom_models"]);
        assert_eq!(project.seed_paths, vec!["custom_seeds"]);
        assert_eq!(project.snapshot_paths, vec!["custom_snapshots"]);
        assert_eq!(project.test_paths, vec!["custom_tests"]);
        assert_eq!(project.macro_paths, vec!["custom_macros"]);
        assert_eq!(project.analysis_paths, vec!["custom_analyses"]);
    }

    #[test]
    fn test_load_renders_var_default_for_project_field() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("dbt_project.yml"),
            "name: test_project\nmodel-paths: [\"{{ var('missing_dir', 'fallback_models') }}\"]\n",
        )
        .unwrap();
        fs::write(tmp.path().join("vars.yml"), "vars: {}\n").unwrap();

        let project = DbtProject::load(tmp.path()).unwrap();
        assert_eq!(project.model_paths, vec!["fallback_models"]);
    }

    #[test]
    fn test_load_missing_required_project_var_has_field_context() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("dbt_project.yml"),
            "name: test_project\nmodel-paths: [\"{{ var('missing_dir') }}\"]\n",
        )
        .unwrap();

        let error = DbtProject::load(tmp.path()).unwrap_err();
        let message = error
            .to_string()
            .replace(tmp.path().to_str().unwrap(), "<project-dir>")
            .replace('\\', "/");
        insta::assert_snapshot!(message, @r###"
failed to render model-paths[0] in <project-dir>/dbt_project.yml: undefined value: required variable 'missing_dir' is not defined (in <string>:1)
        "###);
    }

    #[test]
    fn test_project_vars_are_not_used_to_render_project_fields() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("dbt_project.yml"),
            "name: test_project\nmodel-paths: [\"{{ var('model_dir') }}\"]\nvars:\n  model_dir: project_models\n",
        )
        .unwrap();

        let error = DbtProject::load(tmp.path()).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<DbtLineageError>(),
            Some(DbtLineageError::ProjectFieldRenderError { field, .. })
                if field == "model-paths[0]"
        ));
    }

    #[test]
    fn test_load_vars_yml_falls_back_to_project_vars_when_empty_or_missing() {
        for vars_yml in ["", "other_top_level_key: ignored\n", "vars: {}\n"] {
            let tmp = tempfile::tempdir().unwrap();
            fs::write(
                tmp.path().join("dbt_project.yml"),
                "name: test_project\nvars:\n  source: project\n",
            )
            .unwrap();
            fs::write(tmp.path().join("vars.yml"), vars_yml).unwrap();

            let project = DbtProject::load(tmp.path()).unwrap();
            assert_eq!(
                project.vars,
                HashMap::from([(
                    "source".to_string(),
                    serde_json::Value::String("project".to_string())
                ),])
            );
        }
    }

    #[test]
    fn test_load_vars_yml_conflicts_with_non_empty_project_vars() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("dbt_project.yml"),
            "name: \"{{ var('missing') }}\"\nmodel-paths: [42]\nvars:\n  source: project\n",
        )
        .unwrap();
        fs::write(tmp.path().join("vars.yml"), "vars:\n  source: file\n").unwrap();

        let err = DbtProject::load(tmp.path()).unwrap_err();
        assert!(matches!(
            err.downcast_ref::<DbtLineageError>(),
            Some(DbtLineageError::ProjectVarsConflict)
        ));
    }

    #[test]
    fn test_load_duplicate_keys() {
        // Test that duplicate YAML keys are accepted (last wins, matching PyYAML behavior).
        // dbt users may have duplicate keys in dbt_project.yml (e.g. vars sections).
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("dbt_project.yml"),
            "name: my_project\nversion: '1.0.0'\nname: my_project_dup\n",
        )
        .unwrap();

        let project = DbtProject::load(tmp.path()).unwrap();
        assert_eq!(project.name, "my_project_dup"); // last wins
    }

    #[test]
    fn test_load_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let err = DbtProject::load(tmp.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("dbt project not found"), "Got: {}", msg);
    }

    #[test]
    fn test_load_invalid_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("dbt_project.yml"), ": : : bad yaml").unwrap();
        let err = DbtProject::load(tmp.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Failed to parse"), "Got: {}", msg);
    }

    #[test]
    fn test_resolve_paths() {
        let yaml = "name: my_project\n";
        let project: DbtProject = serde_saphyr::from_str(yaml).unwrap();
        let base = Path::new("/proj");
        let paths = project.resolve_paths(base);
        // resolve_paths normalizes paths, so expected values must also be normalized
        let expected = |name: &str| vec![normalize_path(&base.join(name))];
        assert_eq!(paths.model_paths, expected("models"));
        assert_eq!(paths.seed_paths, expected("seeds"));
        assert_eq!(paths.snapshot_paths, expected("snapshots"));
        assert_eq!(paths.test_paths, expected("tests"));
        assert_eq!(paths.macro_paths, expected("macros"));
        assert_eq!(paths.analysis_paths, expected("analyses"));
    }
}
