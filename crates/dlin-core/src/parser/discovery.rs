use anyhow::Result;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use super::project::{DispatchOverrides, ResolvedPaths, is_sql_file};
use crate::graph::dependency::{
    ConfigDiagnostic, InputFailure, InputKind, InputStage, PathOrigin, PathSetting, SettingPath,
    relative_display_path,
};

/// All discovered files in the dbt project, categorized by type
#[derive(Debug, Default)]
pub struct DiscoveredFiles {
    pub model_sql_files: Vec<PathBuf>,
    pub seed_files: Vec<PathBuf>,
    pub snapshot_sql_files: Vec<PathBuf>,
    pub test_sql_files: Vec<PathBuf>,
    pub yaml_files: Vec<PathBuf>,
    pub macro_sql_files: Vec<PathBuf>,
    /// Whether discovery could not fully inspect the root's macro search paths.
    pub macro_inventory_incomplete: bool,
    /// Project dispatch settings that can change the default macro search order.
    pub dispatch_overrides: DispatchOverrides,
    /// Inputs that could not be inspected or listed. Discovery carried on
    /// without them. Sorted and deduplicated.
    pub skipped_inputs: Vec<SkippedInput>,
    /// Configured directories that do not exist. Sorted and deduplicated.
    pub missing_paths: Vec<MissingPath>,
}

impl DiscoveredFiles {
    /// Number of files found, regardless of whether they can be read or
    /// parsed later.
    pub fn found_file_count(&self) -> usize {
        self.model_sql_files.len()
            + self.seed_files.len()
            + self.snapshot_sql_files.len()
            + self.test_sql_files.len()
            + self.yaml_files.len()
            + self.macro_sql_files.len()
    }
}

/// An input that was skipped, with an absolute path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SkippedInput {
    pub path: PathBuf,
    pub kind: InputKind,
    pub stage: InputStage,
    pub failure: InputFailure,
}

/// A configured directory that does not exist, with an absolute path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MissingPath {
    pub setting: PathSetting,
    pub path: PathBuf,
    pub origin: PathOrigin,
}

/// Files found under one root, before they are assigned to a resource type.
#[derive(Debug, Default)]
struct WalkedFiles {
    sql: Vec<PathBuf>,
    yaml: Vec<PathBuf>,
    csv: Vec<PathBuf>,
}

/// Walk all configured paths and collect SQL/YAML files.
///
/// A configured directory that does not exist is recorded in
/// `missing_paths` and treated as empty, as dbt does. A configured path that
/// cannot be inspected, is not a directory, or cannot be fully listed is
/// recorded in `skipped_inputs`.
///
/// SQL files under `generic/` in a test path define generic tests, so they
/// are macro files, as in dbt. They come after the files of `macro-paths`.
pub fn discover_files(paths: &ResolvedPaths) -> Result<DiscoveredFiles> {
    let mut discovered = DiscoveredFiles {
        dispatch_overrides: paths.dispatch_overrides.clone(),
        ..DiscoveredFiles::default()
    };
    let mut generic_test_sql_files = Vec::new();

    for (setting, dirs, origin) in paths.settings() {
        for dir in dirs {
            let skipped_before = discovered.skipped_inputs.len();
            let walked = walk_root(
                setting,
                dir,
                origin,
                paths.allow_jinja_file_extensions,
                &mut discovered,
            );
            if matches!(setting, PathSetting::MacroPaths | PathSetting::TestPaths)
                && discovered.skipped_inputs.len() != skipped_before
            {
                discovered.macro_inventory_incomplete = true;
            }
            let Some(walked) = walked else {
                continue;
            };
            match setting {
                // Analyses are treated as models, consistent with manifest mode.
                PathSetting::ModelPaths | PathSetting::AnalysisPaths => {
                    discovered.model_sql_files.extend(walked.sql);
                    discovered.yaml_files.extend(walked.yaml);
                }
                PathSetting::SeedPaths => {
                    // Seeds are CSV files; their YAML properties are collected too.
                    discovered.seed_files.extend(walked.csv);
                    discovered.yaml_files.extend(walked.yaml);
                }
                PathSetting::SnapshotPaths => {
                    discovered.snapshot_sql_files.extend(walked.sql);
                    discovered.yaml_files.extend(walked.yaml);
                }
                PathSetting::TestPaths => {
                    let (generic, singular): (Vec<_>, Vec<_>) = walked
                        .sql
                        .into_iter()
                        .partition(|path| is_generic_test_definition(dir, path));
                    generic_test_sql_files.extend(generic);
                    discovered.test_sql_files.extend(singular);
                    discovered.yaml_files.extend(walked.yaml);
                }
                PathSetting::MacroPaths => {
                    discovered.macro_sql_files.extend(walked.sql);
                }
            }
        }
    }

    discovered.macro_sql_files.extend(generic_test_sql_files);
    // A file is one macro file even when it is under both a macro path and
    // a test path's `generic/`. The first occurrence keeps its precedence.
    let mut seen = std::collections::HashSet::new();
    discovered
        .macro_sql_files
        .retain(|path| seen.insert(path.clone()));

    discovered.skipped_inputs.sort();
    discovered.skipped_inputs.dedup();
    discovered.missing_paths.sort();
    discovered.missing_paths.dedup();
    Ok(discovered)
}

/// Whether a SQL file in a test path is under the `generic/` directory at
/// the root of that test path, where dbt reads generic test definitions.
/// `tests/unit/generic/x.sql` is not: only the first component counts.
fn is_generic_test_definition(test_root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(test_root) else {
        return false;
    };
    let mut components = relative.components();
    components
        .next()
        .is_some_and(|first| first.as_os_str() == "generic")
        && components.next().is_some()
}

/// Inspect one configured root and walk it. Returns `None` when the root is
/// missing or skipped; the reason is recorded in `discovered`.
fn walk_root(
    setting: PathSetting,
    dir: &Path,
    origin: PathOrigin,
    allow_jinja_file_extensions: bool,
    discovered: &mut DiscoveredFiles,
) -> Option<WalkedFiles> {
    match std::fs::metadata(dir) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            discovered.skipped_inputs.push(SkippedInput {
                path: dir.to_path_buf(),
                kind: InputKind::Directory,
                stage: InputStage::Metadata,
                failure: InputFailure::NotADirectory,
            });
            return None;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            discovered.missing_paths.push(MissingPath {
                setting,
                path: dir.to_path_buf(),
                origin,
            });
            return None;
        }
        Err(error) => {
            discovered.skipped_inputs.push(SkippedInput {
                path: dir.to_path_buf(),
                kind: InputKind::Directory,
                stage: InputStage::Metadata,
                failure: InputFailure::from_io_error(&error),
            });
            return None;
        }
    }
    let (walked, skipped) = walk_directory(dir, allow_jinja_file_extensions);
    discovered.skipped_inputs.extend(skipped);
    Some(walked)
}

/// Walk an existing directory. Entries that cannot be listed are returned as
/// skipped inputs instead of being dropped.
fn walk_directory(
    dir: &Path,
    allow_jinja_file_extensions: bool,
) -> (WalkedFiles, Vec<SkippedInput>) {
    let mut walked = WalkedFiles::default();
    let mut skipped = Vec::new();

    // Sorting makes the discovered order independent of the filesystem, so
    // that the macro chosen among same-named ones and the diagnostics are
    // deterministic.
    for entry in WalkDir::new(dir).sort_by_file_name() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                skipped.push(SkippedInput {
                    path: error.path().unwrap_or(dir).to_path_buf(),
                    kind: InputKind::Directory,
                    stage: InputStage::Enumerate,
                    failure: error
                        .io_error()
                        .map(InputFailure::from_io_error)
                        .unwrap_or(InputFailure::IoError),
                });
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if is_sql_file(path, allow_jinja_file_extensions) {
            walked.sql.push(path.to_path_buf());
        } else {
            match path.extension().and_then(|e| e.to_str()) {
                Some("yml" | "yaml") => walked.yaml.push(path.to_path_buf()),
                Some("csv") => walked.csv.push(path.to_path_buf()),
                _ => {}
            }
        }
    }

    (walked, skipped)
}

/// Decide the configuration diagnostics for a discovered project.
///
/// At most one warning is returned, chosen in this order: configured paths
/// that do not exist, then default `model-paths` that do not exist, then no
/// input file found at all. The last one is not raised when a path could not
/// be inspected or listed, since that is a gap rather than an empty project.
/// Every default path that does not exist is also returned as an info
/// diagnostic.
pub fn config_diagnostics(
    paths: &ResolvedPaths,
    files: &DiscoveredFiles,
    project_dir: &Path,
    project_name: &str,
) -> Vec<ConfigDiagnostic> {
    let base = crate::input::normalize_path(project_dir);
    let display = |path: &Path| relative_display_path(path, &base);
    let setting_path = |missing: &MissingPath| SettingPath {
        setting: missing.setting,
        path: display(&missing.path),
    };

    // Compare by membership rather than by count: a setting may list the
    // same directory twice.
    let all_model_paths_missing = !paths.model_paths.is_empty()
        && paths.model_paths.iter().all(|path| {
            files
                .missing_paths
                .iter()
                .any(|missing| missing.setting == PathSetting::ModelPaths && &missing.path == path)
        });

    let mut diagnostics = Vec::new();
    let explicit: Vec<SettingPath> = files
        .missing_paths
        .iter()
        .filter(|missing| missing.origin == PathOrigin::Explicit)
        .map(setting_path)
        .collect();
    if !explicit.is_empty() {
        diagnostics.push(ConfigDiagnostic::ConfiguredInputPathMissing {
            paths: explicit,
            all_model_paths_missing,
        });
    } else if all_model_paths_missing {
        diagnostics.push(ConfigDiagnostic::ModelPathsMissing {
            paths: paths.model_paths.iter().map(|path| display(path)).collect(),
        });
    } else if files.found_file_count() == 0 && files.skipped_inputs.is_empty() {
        diagnostics.push(ConfigDiagnostic::ProjectInputsEmpty {
            project_name: project_name.to_string(),
            searched: paths
                .settings()
                .into_iter()
                .flat_map(|(setting, dirs, _)| {
                    dirs.iter().map(move |dir| SettingPath {
                        setting,
                        path: display(dir),
                    })
                })
                .collect(),
        });
    }

    diagnostics.extend(
        files
            .missing_paths
            .iter()
            .filter(|missing| missing.origin == PathOrigin::Default)
            .map(|missing| ConfigDiagnostic::DefaultInputPathMissing {
                setting: missing.setting,
                path: display(&missing.path),
            }),
    );
    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::project::DbtProject;
    use std::fs;

    #[test]
    fn test_walk_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let models_dir = tmp.path().join("models");
        fs::create_dir_all(&models_dir).unwrap();
        fs::write(models_dir.join("model_a.sql"), "SELECT 1").unwrap();
        fs::write(models_dir.join("schema.yml"), "version: 2").unwrap();
        fs::write(models_dir.join("readme.md"), "# Readme").unwrap();

        let (walked, skipped) = walk_directory(&models_dir, false);
        assert_eq!(walked.sql.len(), 1);
        assert_eq!(walked.yaml.len(), 1);
        assert!(skipped.is_empty());
    }

    #[test]
    fn test_walk_csv_files() {
        let tmp = tempfile::tempdir().unwrap();
        let seeds_dir = tmp.path().join("seeds");
        fs::create_dir_all(&seeds_dir).unwrap();
        fs::write(seeds_dir.join("countries.csv"), "id,name\n1,US").unwrap();
        fs::write(seeds_dir.join("schema.yml"), "version: 2").unwrap();
        fs::write(seeds_dir.join("notes.txt"), "notes").unwrap();

        let (walked, _) = walk_directory(&seeds_dir, false);
        assert_eq!(walked.csv.len(), 1);
        assert!(walked.csv[0].ends_with("countries.csv"));
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

        let (walked, _) = walk_directory(&models_dir, false);
        assert_eq!(walked.sql.len(), 2);
        assert_eq!(walked.yaml.len(), 1);
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
            dispatch_overrides: Default::default(),
            origins: Default::default(),
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
            dispatch_overrides: Default::default(),
            origins: Default::default(),
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
            dispatch_overrides: Default::default(),
            origins: Default::default(),
        };

        let discovered = discover_files(&paths).unwrap();
        assert_eq!(discovered.model_sql_files.len(), 1);
    }

    /// SQL files under `generic/` at the root of a test path define generic
    /// tests: they are macro files after those of `macro-paths`, not singular
    /// tests. YAML there is still read.
    #[test]
    fn test_discover_files_generic_test_definitions_are_macros() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for path in [
            "macros/helpers.sql",
            "tests/assert_positive.sql",
            "tests/generic/test_is_positive.sql",
            "tests/generic/nested/test_is_even.sql",
            "tests/generic/schema.yml",
            "tests/unit/generic/assert_unit.sql",
            "tests/generic_checks/assert_generic.sql",
            "custom/checks/generic/test_is_odd.sql",
            "custom/checks/assert_custom.sql",
        ] {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "select 1").unwrap();
        }
        let paths = ResolvedPaths {
            model_paths: vec![],
            seed_paths: vec![],
            snapshot_paths: vec![],
            test_paths: vec![root.join("tests"), root.join("custom/checks")],
            // A macro path that is also a test path's `generic/` lists its
            // files once.
            macro_paths: vec![root.join("macros"), root.join("tests/generic")],
            analysis_paths: vec![],
            allow_jinja_file_extensions: false,
            dispatch_overrides: Default::default(),
            origins: Default::default(),
        };

        let discovered = discover_files(&paths).unwrap();
        let relative = |files: &[PathBuf]| -> Vec<PathBuf> {
            files
                .iter()
                .map(|path| path.strip_prefix(root).unwrap().to_path_buf())
                .collect()
        };
        assert_eq!(
            relative(&discovered.test_sql_files),
            [
                "tests/assert_positive.sql",
                "tests/generic_checks/assert_generic.sql",
                "tests/unit/generic/assert_unit.sql",
                "custom/checks/assert_custom.sql",
            ]
            .map(PathBuf::from)
        );
        assert_eq!(
            relative(&discovered.macro_sql_files),
            [
                "macros/helpers.sql",
                "tests/generic/nested/test_is_even.sql",
                "tests/generic/test_is_positive.sql",
                "custom/checks/generic/test_is_odd.sql",
            ]
            .map(PathBuf::from)
        );
        assert_eq!(
            relative(&discovered.yaml_files),
            [PathBuf::from("tests/generic/schema.yml")]
        );
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
            dispatch_overrides: Default::default(),
            origins: Default::default(),
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
            dispatch_overrides: Default::default(),
            origins: Default::default(),
        };

        let discovered = discover_files(&paths).unwrap();
        assert!(discovered.model_sql_files.is_empty());
        assert!(discovered.seed_files.is_empty());
        assert!(discovered.snapshot_sql_files.is_empty());
        assert!(discovered.test_sql_files.is_empty());
        assert!(discovered.yaml_files.is_empty());
    }

    fn load_project(dbt_project_yml: &str) -> (tempfile::TempDir, ResolvedPaths, DbtProject) {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("dbt_project.yml"), dbt_project_yml).unwrap();
        let project = DbtProject::load(tmp.path()).unwrap();
        let paths = project.resolve_paths(tmp.path());
        (tmp, paths, project)
    }

    fn diagnostics_for(tmp: &tempfile::TempDir, paths: &ResolvedPaths) -> Vec<ConfigDiagnostic> {
        let files = discover_files(paths).unwrap();
        config_diagnostics(paths, &files, tmp.path(), "p")
    }

    fn warnings(diagnostics: &[ConfigDiagnostic]) -> Vec<&ConfigDiagnostic> {
        diagnostics
            .iter()
            .filter(|d| d.level() == crate::graph::dependency::ConfigDiagnosticLevel::Warning)
            .collect()
    }

    #[test]
    fn test_missing_dirs_are_recorded_with_origin_not_skipped() {
        let (tmp, paths, _) = load_project("name: p\nmodel-paths: [\"models\", \"extra\"]\n");
        fs::create_dir_all(tmp.path().join("models")).unwrap();
        let discovered = discover_files(&paths).unwrap();
        assert!(discovered.skipped_inputs.is_empty());
        let missing: Vec<_> = discovered
            .missing_paths
            .iter()
            .map(|m| (m.setting, m.path.file_name().unwrap().to_owned(), m.origin))
            .collect();
        assert!(missing.contains(&(
            PathSetting::ModelPaths,
            "extra".into(),
            PathOrigin::Explicit
        )));
        assert!(missing.contains(&(PathSetting::SeedPaths, "seeds".into(), PathOrigin::Default)));
        assert!(
            !missing
                .iter()
                .any(|(_, name, _)| name.to_str() == Some("models"))
        );
    }

    #[test]
    fn test_root_that_is_a_file_is_skipped_as_not_a_directory() {
        let (tmp, paths, _) = load_project("name: p\nmodel-paths: [\"models.sql\"]\n");
        fs::write(tmp.path().join("models.sql"), "select 1").unwrap();
        let discovered = discover_files(&paths).unwrap();
        assert!(discovered.model_sql_files.is_empty());
        assert_eq!(discovered.skipped_inputs.len(), 1);
        let skipped = &discovered.skipped_inputs[0];
        assert_eq!(skipped.kind, InputKind::Directory);
        assert_eq!(skipped.stage, InputStage::Metadata);
        assert_eq!(skipped.failure, InputFailure::NotADirectory);
        // A gap, not an empty project.
        assert!(warnings(&config_diagnostics(&paths, &discovered, tmp.path(), "p")).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn test_unreadable_subdirectory_is_skipped_during_enumeration() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let models = tmp.path().join("models");
        let locked = models.join("locked");
        fs::create_dir_all(&locked).unwrap();
        fs::write(models.join("a.sql"), "select 1").unwrap();
        fs::write(locked.join("b.sql"), "select 1").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let readable = fs::read_dir(&locked).is_ok();
        let (walked, skipped) = walk_directory(&models, false);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        if readable {
            // Running with privileges that ignore permissions.
            return;
        }
        assert_eq!(walked.sql.len(), 1);
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].path, locked);
        assert_eq!(skipped[0].kind, InputKind::Directory);
        assert_eq!(skipped[0].stage, InputStage::Enumerate);
        assert_eq!(skipped[0].failure, InputFailure::PermissionDenied);
    }

    #[test]
    fn test_config_diagnostics_default_project_with_models_has_no_warning() {
        let (tmp, paths, _) = load_project("name: p\n");
        fs::create_dir_all(tmp.path().join("models")).unwrap();
        fs::write(tmp.path().join("models/a.sql"), "select 1").unwrap();
        let diagnostics = diagnostics_for(&tmp, &paths);
        assert!(warnings(&diagnostics).is_empty());
        let infos: Vec<_> = diagnostics.iter().map(ConfigDiagnostic::code).collect();
        assert_eq!(infos.len(), 5, "{diagnostics:?}");
        assert!(
            infos
                .iter()
                .all(|code| *code == "default_input_path_missing")
        );
        assert!(
            diagnostics.contains(&ConfigDiagnostic::DefaultInputPathMissing {
                setting: PathSetting::AnalysisPaths,
                path: "analyses".into(),
            })
        );
    }

    #[test]
    fn test_config_diagnostics_explicit_missing_paths_take_priority() {
        let (tmp, paths, _) =
            load_project("name: p\nmodel-paths: [\"gone\"]\ntest-paths: [\"../shared/tests\"]\n");
        let diagnostics = diagnostics_for(&tmp, &paths);
        assert_eq!(
            warnings(&diagnostics),
            vec![&ConfigDiagnostic::ConfiguredInputPathMissing {
                paths: vec![
                    SettingPath {
                        setting: PathSetting::ModelPaths,
                        path: "gone".into(),
                    },
                    SettingPath {
                        setting: PathSetting::TestPaths,
                        path: "../shared/tests".into(),
                    },
                ],
                all_model_paths_missing: true,
            }]
        );
    }

    #[test]
    fn test_config_diagnostics_explicit_missing_with_existing_models() {
        let (tmp, paths, _) = load_project("name: p\nmodel-paths: [\"models\", \"gone\"]\n");
        fs::create_dir_all(tmp.path().join("models")).unwrap();
        let diagnostics = diagnostics_for(&tmp, &paths);
        assert_eq!(
            warnings(&diagnostics),
            vec![&ConfigDiagnostic::ConfiguredInputPathMissing {
                paths: vec![SettingPath {
                    setting: PathSetting::ModelPaths,
                    path: "gone".into(),
                }],
                all_model_paths_missing: false,
            }]
        );
    }

    #[test]
    fn test_config_diagnostics_default_model_paths_missing() {
        let (tmp, paths, _) = load_project("name: p\n");
        fs::create_dir_all(tmp.path().join("macros")).unwrap();
        fs::write(
            tmp.path().join("macros/m.sql"),
            "{% macro m() %}{% endmacro %}",
        )
        .unwrap();
        let diagnostics = diagnostics_for(&tmp, &paths);
        assert_eq!(
            warnings(&diagnostics),
            vec![&ConfigDiagnostic::ModelPathsMissing {
                paths: vec!["models".into()],
            }]
        );
    }

    #[test]
    fn test_config_diagnostics_repeated_model_path() {
        let (tmp, paths, _) = load_project("name: p\nmodel-paths: [\"gone\", \"gone\"]\n");
        let diagnostics = diagnostics_for(&tmp, &paths);
        let warnings = warnings(&diagnostics);
        assert!(
            matches!(
                warnings.as_slice(),
                [ConfigDiagnostic::ConfiguredInputPathMissing {
                    all_model_paths_missing: true,
                    ..
                }]
            ),
            "{warnings:?}"
        );
    }

    #[test]
    fn test_config_diagnostics_no_project_inputs_empty_when_inputs_were_skipped() {
        let (tmp, paths, _) = load_project("name: p\nmodel-paths: []\n");
        let mut files = discover_files(&paths).unwrap();
        files.skipped_inputs.push(SkippedInput {
            path: tmp.path().join("macros/locked"),
            kind: InputKind::Directory,
            stage: InputStage::Enumerate,
            failure: InputFailure::PermissionDenied,
        });
        let diagnostics = config_diagnostics(&paths, &files, tmp.path(), "p");
        assert!(warnings(&diagnostics).is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn test_config_diagnostics_project_inputs_empty() {
        let (tmp, paths, _) = load_project("name: p\nmodel-paths: []\n");
        let diagnostics = diagnostics_for(&tmp, &paths);
        let warnings = warnings(&diagnostics);
        assert_eq!(warnings.len(), 1);
        let ConfigDiagnostic::ProjectInputsEmpty {
            project_name,
            searched,
        } = warnings[0]
        else {
            panic!("unexpected {warnings:?}");
        };
        assert_eq!(project_name, "p");
        assert!(searched.contains(&SettingPath {
            setting: PathSetting::MacroPaths,
            path: "macros".into(),
        }));
    }

    #[test]
    fn test_config_diagnostics_empty_existing_directory_is_project_inputs_empty() {
        let (tmp, paths, _) = load_project("name: p\n");
        fs::create_dir_all(tmp.path().join("models")).unwrap();
        let diagnostics = diagnostics_for(&tmp, &paths);
        assert_eq!(
            warnings(&diagnostics)
                .iter()
                .map(|d| d.code())
                .collect::<Vec<_>>(),
            vec!["project_inputs_empty"]
        );
    }
}
