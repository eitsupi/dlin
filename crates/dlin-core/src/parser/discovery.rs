use anyhow::Result;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use super::project::{ResolvedPaths, is_sql_file};

/// All discovered files in the dbt project, categorized by type
#[derive(Debug, Default)]
pub struct DiscoveredFiles {
    pub model_sql_files: Vec<PathBuf>,
    pub seed_files: Vec<PathBuf>,
    pub snapshot_sql_files: Vec<PathBuf>,
    pub test_sql_files: Vec<PathBuf>,
    pub yaml_files: Vec<PathBuf>,
    pub macro_sql_files: Vec<PathBuf>,
}

/// Walk all configured paths and collect SQL/YAML files
pub fn discover_files(paths: &ResolvedPaths) -> Result<DiscoveredFiles> {
    let mut discovered = DiscoveredFiles::default();

    // Models
    for dir in &paths.model_paths {
        let (sql, yaml) = walk_directory(dir, paths.allow_jinja_file_extensions);
        discovered.model_sql_files.extend(sql);
        discovered.yaml_files.extend(yaml);
    }

    // Seeds
    for dir in &paths.seed_paths {
        let (_, yaml) = walk_directory(dir, paths.allow_jinja_file_extensions);
        // Seeds are CSV files typically, but we collect their YAML schema files
        discovered.yaml_files.extend(yaml);
        // Also look for .csv files
        discovered.seed_files.extend(walk_csv_files(dir));
    }

    // Snapshots
    for dir in &paths.snapshot_paths {
        let (sql, yaml) = walk_directory(dir, paths.allow_jinja_file_extensions);
        discovered.snapshot_sql_files.extend(sql);
        discovered.yaml_files.extend(yaml);
    }

    // Tests
    for dir in &paths.test_paths {
        let (sql, yaml) = walk_directory(dir, paths.allow_jinja_file_extensions);
        discovered.test_sql_files.extend(sql);
        discovered.yaml_files.extend(yaml);
    }

    // Analyses (treated as models, consistent with manifest mode)
    for dir in &paths.analysis_paths {
        let (sql, yaml) = walk_directory(dir, paths.allow_jinja_file_extensions);
        discovered.model_sql_files.extend(sql);
        discovered.yaml_files.extend(yaml);
    }

    // Macros
    for dir in &paths.macro_paths {
        let (sql, _yaml) = walk_directory(dir, paths.allow_jinja_file_extensions);
        discovered.macro_sql_files.extend(sql);
    }

    Ok(discovered)
}

/// Walk a directory and return (sql_files, yaml_files)
fn walk_directory(dir: &Path, allow_jinja_file_extensions: bool) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut sql_files = Vec::new();
    let mut yaml_files = Vec::new();

    if !dir.exists() {
        return (sql_files, yaml_files);
    }

    for entry in WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path();
        if is_sql_file(path, allow_jinja_file_extensions) {
            sql_files.push(path.to_path_buf());
        } else if let Some("yml" | "yaml") = path.extension().and_then(|e| e.to_str()) {
            yaml_files.push(path.to_path_buf());
        }
    }

    (sql_files, yaml_files)
}

/// Walk a directory and return CSV files (for seeds)
fn walk_csv_files(dir: &Path) -> Vec<PathBuf> {
    if !dir.exists() {
        return Vec::new();
    }

    WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("csv"))
        .map(|e| e.path().to_path_buf())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::project::DbtProject;
    use std::fs;

    #[test]
    fn test_walk_nonexistent_directory() {
        let (sql, yaml) = walk_directory(Path::new("/nonexistent/path"), false);
        assert!(sql.is_empty());
        assert!(yaml.is_empty());
    }

    #[test]
    fn test_walk_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let models_dir = tmp.path().join("models");
        fs::create_dir_all(&models_dir).unwrap();
        fs::write(models_dir.join("model_a.sql"), "SELECT 1").unwrap();
        fs::write(models_dir.join("schema.yml"), "version: 2").unwrap();
        fs::write(models_dir.join("readme.md"), "# Readme").unwrap();

        let (sql, yaml) = walk_directory(&models_dir, false);
        assert_eq!(sql.len(), 1);
        assert_eq!(yaml.len(), 1);
    }

    #[test]
    fn test_walk_csv_files() {
        let tmp = tempfile::tempdir().unwrap();
        let seeds_dir = tmp.path().join("seeds");
        fs::create_dir_all(&seeds_dir).unwrap();
        fs::write(seeds_dir.join("countries.csv"), "id,name\n1,US").unwrap();
        fs::write(seeds_dir.join("schema.yml"), "version: 2").unwrap();
        fs::write(seeds_dir.join("notes.txt"), "notes").unwrap();

        let csv_files = walk_csv_files(&seeds_dir);
        assert_eq!(csv_files.len(), 1);
        assert!(csv_files[0].ends_with("countries.csv"));
    }

    #[test]
    fn test_walk_csv_files_nonexistent() {
        let csv_files = walk_csv_files(Path::new("/nonexistent/path"));
        assert!(csv_files.is_empty());
    }

    #[test]
    fn test_walk_directory_nested() {
        let tmp = tempfile::tempdir().unwrap();
        let models_dir = tmp.path().join("models");
        let staging_dir = models_dir.join("staging");
        fs::create_dir_all(&staging_dir).unwrap();
        fs::write(staging_dir.join("stg_a.sql"), "SELECT 1").unwrap();
        fs::write(staging_dir.join("stg_b.sql"), "SELECT 2").unwrap();
        fs::write(models_dir.join("schema.yaml"), "version: 2").unwrap();

        let (sql, yaml) = walk_directory(&models_dir, false);
        assert_eq!(sql.len(), 2);
        assert_eq!(yaml.len(), 1);
    }

    #[test]
    fn test_discover_files_full() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path();

        // Models
        let models_dir = project_dir.join("models");
        fs::create_dir_all(&models_dir).unwrap();
        fs::write(models_dir.join("model_a.sql"), "SELECT 1").unwrap();
        fs::write(models_dir.join("schema.yml"), "version: 2").unwrap();

        // Seeds
        let seeds_dir = project_dir.join("seeds");
        fs::create_dir_all(&seeds_dir).unwrap();
        fs::write(seeds_dir.join("seed.csv"), "a,b\n1,2").unwrap();

        // Snapshots
        let snap_dir = project_dir.join("snapshots");
        fs::create_dir_all(&snap_dir).unwrap();
        fs::write(snap_dir.join("snap.sql"), "SELECT 1").unwrap();

        // Tests
        let test_dir = project_dir.join("tests");
        fs::create_dir_all(&test_dir).unwrap();
        fs::write(test_dir.join("test_a.sql"), "SELECT 1").unwrap();

        let paths = ResolvedPaths {
            model_paths: vec![models_dir],
            seed_paths: vec![seeds_dir],
            snapshot_paths: vec![snap_dir],
            test_paths: vec![test_dir],
            macro_paths: vec![],
            analysis_paths: vec![],
            allow_jinja_file_extensions: false,
        };

        let discovered = discover_files(&paths).unwrap();
        assert_eq!(discovered.model_sql_files.len(), 1);
        assert_eq!(discovered.seed_files.len(), 1);
        assert_eq!(discovered.snapshot_sql_files.len(), 1);
        assert_eq!(discovered.test_sql_files.len(), 1);
        assert_eq!(discovered.yaml_files.len(), 1);
    }

    #[test]
    fn test_discover_files_with_analyses() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path();

        // Models
        let models_dir = project_dir.join("models");
        fs::create_dir_all(&models_dir).unwrap();
        fs::write(models_dir.join("model_a.sql"), "SELECT 1").unwrap();

        // Analyses (should be treated as models)
        let analyses_dir = project_dir.join("analyses");
        fs::create_dir_all(&analyses_dir).unwrap();
        fs::write(analyses_dir.join("my_analysis.sql"), "SELECT 1").unwrap();
        fs::write(analyses_dir.join("schema.yml"), "version: 2").unwrap();

        let paths = ResolvedPaths {
            model_paths: vec![models_dir],
            seed_paths: vec![],
            snapshot_paths: vec![],
            test_paths: vec![],
            macro_paths: vec![],
            analysis_paths: vec![analyses_dir],
            allow_jinja_file_extensions: false,
        };

        let discovered = discover_files(&paths).unwrap();
        // Analysis SQL files are added to model_sql_files
        assert_eq!(discovered.model_sql_files.len(), 2);
        // Analysis YAML files are collected
        assert_eq!(discovered.yaml_files.len(), 1);
    }

    #[test]
    fn test_discover_files_uses_vars_yml_for_project_model_path() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path();
        let models_dir = project_dir.join("custom_models");
        fs::create_dir_all(&models_dir).unwrap();
        fs::write(models_dir.join("orders.sql"), "select 1").unwrap();
        fs::write(
            project_dir.join("dbt_project.yml"),
            "name: test_project\nmodel-paths: [\"{{ var('model_dir') }}\"]\n",
        )
        .unwrap();
        fs::write(
            project_dir.join("vars.yml"),
            "vars:\n  model_dir: custom_models\n",
        )
        .unwrap();

        let project = DbtProject::load(project_dir).unwrap();
        let discovered = discover_files(&project.resolve_paths(project_dir)).unwrap();

        let expected_model = crate::input::normalize_path(&models_dir.join("orders.sql"));
        assert_eq!(discovered.model_sql_files, vec![expected_model]);
    }

    #[test]
    fn test_discover_files_accepts_jinja_sql_suffix_when_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        let models_dir = tmp.path().join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        std::fs::write(models_dir.join("orders.sql.jinja"), "select 1").unwrap();
        for filename in [
            "ignored.j2",
            "ignored.md.jinja",
            "ignored.sql.other",
            "ignored.sql.jinja.j2",
        ] {
            std::fs::write(models_dir.join(filename), "select 1").unwrap();
        }
        let paths = ResolvedPaths {
            model_paths: vec![models_dir],
            seed_paths: vec![],
            snapshot_paths: vec![],
            test_paths: vec![],
            macro_paths: vec![],
            analysis_paths: vec![],
            allow_jinja_file_extensions: true,
        };

        let discovered = discover_files(&paths).unwrap();
        assert_eq!(discovered.model_sql_files.len(), 1);
    }

    #[test]
    fn test_discover_files_accepts_suffix_for_all_sql_resource_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = ["models", "snapshots", "tests", "analyses", "macros"];
        for dir in dirs {
            let path = tmp.path().join(dir);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join(format!("{dir}.sql.j2")), "select 1").unwrap();
        }
        let paths = ResolvedPaths {
            model_paths: vec![tmp.path().join("models")],
            seed_paths: vec![],
            snapshot_paths: vec![tmp.path().join("snapshots")],
            test_paths: vec![tmp.path().join("tests")],
            macro_paths: vec![tmp.path().join("macros")],
            analysis_paths: vec![tmp.path().join("analyses")],
            allow_jinja_file_extensions: true,
        };
        let discovered = discover_files(&paths).unwrap();
        assert_eq!(discovered.model_sql_files.len(), 2);
        assert_eq!(discovered.snapshot_sql_files.len(), 1);
        assert_eq!(discovered.test_sql_files.len(), 1);
        assert_eq!(discovered.macro_sql_files.len(), 1);
    }

    #[test]
    fn test_discover_files_missing_dirs() {
        let paths = ResolvedPaths {
            model_paths: vec![PathBuf::from("/nonexistent/models")],
            seed_paths: vec![PathBuf::from("/nonexistent/seeds")],
            snapshot_paths: vec![PathBuf::from("/nonexistent/snapshots")],
            test_paths: vec![PathBuf::from("/nonexistent/tests")],
            macro_paths: vec![],
            analysis_paths: vec![],
            allow_jinja_file_extensions: false,
        };

        let discovered = discover_files(&paths).unwrap();
        assert!(discovered.model_sql_files.is_empty());
        assert!(discovered.seed_files.is_empty());
        assert!(discovered.snapshot_sql_files.is_empty());
        assert!(discovered.test_sql_files.is_empty());
        assert!(discovered.yaml_files.is_empty());
    }
}
