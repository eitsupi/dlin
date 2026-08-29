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

        let vars_file = project_dir.join("vars.yml");
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

#[derive(Debug)]
pub struct ResolvedPaths {
    pub model_paths: Vec<PathBuf>,
    pub seed_paths: Vec<PathBuf>,
    pub snapshot_paths: Vec<PathBuf>,
    pub test_paths: Vec<PathBuf>,
    pub macro_paths: Vec<PathBuf>,
    pub analysis_paths: Vec<PathBuf>,
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
