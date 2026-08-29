use anyhow::{Context, Result};
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

        let project: DbtProject =
            super::yaml_from_str(&content, &project_file.display().to_string())
                .context(format!("Failed to parse {}", project_file.display()))?;

        let vars_file = project_dir.join("vars.yml");
        if !vars_file.exists() {
            return Ok(project);
        }

        let vars_content =
            std::fs::read_to_string(&vars_file).map_err(|e| DbtLineageError::FileReadError {
                path: vars_file.clone(),
                source: e,
            })?;
        let vars_file_data: VarsFile =
            super::yaml_from_str(&vars_content, &vars_file.display().to_string())
                .context(format!("Failed to parse {}", vars_file.display()))?;

        let Some(vars) = vars_file_data.vars.filter(|vars| !vars.is_empty()) else {
            return Ok(project);
        };

        if !project.vars.is_empty() {
            return Err(DbtLineageError::ProjectVarsConflict.into());
        }

        Ok(DbtProject { vars, ..project })
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
            "name: test_project\nvars:\n  source: project\n",
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
