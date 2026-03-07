use std::io::{self, BufRead, IsTerminal};
use std::path::{Path, PathBuf};

use crate::graph::types::LineageGraph;
use crate::parser::project::ResolvedPaths;
use crate::parser::yaml_schema;

/// Classification of a stdin input line
enum InputLine {
    /// A .sql file path under dbt project paths (absolute path)
    SqlFile(PathBuf),
    /// A .yml/.yaml file path under dbt project paths (absolute path)
    YamlFile(PathBuf),
    /// A bare name (no extension) treated as model/source name
    ModelName(String),
    /// Ignored (non-dbt extension or file outside dbt project paths)
    Ignore,
}

/// Resolve a potentially relative path to absolute using the given base directory.
/// stdin paths (e.g. from `git diff --name-only`) are relative to the working
/// directory where the command was invoked, which may differ from the dbt project
/// directory when `dbt_project.yml` lives in a subdirectory.
fn to_absolute(path_str: &str, cwd: &Path) -> PathBuf {
    let path = Path::new(path_str);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// Read lines from stdin if it is not a terminal.
/// Returns an empty Vec if stdin is a terminal (interactive mode).
pub fn read_stdin_lines() -> Vec<String> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        return Vec::new();
    }
    stdin
        .lock()
        .lines()
        .filter_map(|l| l.ok())
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .collect()
}

/// Classify a single stdin line based on its extension and whether it falls
/// under one of the dbt project source directories.
fn classify_line(line: &str, resolved_paths: &ResolvedPaths, cwd: &Path) -> InputLine {
    let path = Path::new(line);
    match path.extension().and_then(|e| e.to_str()) {
        Some("sql") => {
            let abs = to_absolute(line, cwd);
            if is_under_dbt_paths(&abs, resolved_paths) {
                InputLine::SqlFile(abs)
            } else {
                InputLine::Ignore
            }
        }
        Some("yml" | "yaml") => {
            let abs = to_absolute(line, cwd);
            if is_under_dbt_paths(&abs, resolved_paths) {
                InputLine::YamlFile(abs)
            } else {
                InputLine::Ignore
            }
        }
        _ => {
            // Lines with a path separator (e.g. "README.md", ".github/ci.yml")
            // that don't match .sql/.yml/.yaml are non-dbt files → ignore.
            // Lines without separators (e.g. "stg_orders", "raw.orders")
            // are treated as model/source names.
            if line.contains('/') || line.contains('\\') {
                InputLine::Ignore
            } else {
                InputLine::ModelName(line.to_string())
            }
        }
    }
}

/// Check if an absolute path falls under any of the configured dbt project directories.
fn is_under_dbt_paths(abs_path: &Path, resolved_paths: &ResolvedPaths) -> bool {
    let all_paths = resolved_paths
        .model_paths
        .iter()
        .chain(&resolved_paths.seed_paths)
        .chain(&resolved_paths.snapshot_paths)
        .chain(&resolved_paths.test_paths)
        .chain(&resolved_paths.analysis_paths);

    all_paths.into_iter().any(|dir| abs_path.starts_with(dir))
}

/// Find a graph node whose `file_path` matches the given absolute path and return its label.
fn resolve_sql_to_label(
    abs_path: &Path,
    graph: &LineageGraph,
    project_dir: &Path,
) -> Option<String> {
    let relative = abs_path.strip_prefix(project_dir).ok()?;

    graph.node_indices().find_map(|idx| {
        let node = &graph[idx];
        if node.file_path.as_deref() == Some(relative) {
            Some(node.label.clone())
        } else {
            None
        }
    })
}

/// Parse a YAML schema file and return source and model names defined in it.
fn expand_yaml_names(abs_path: &Path) -> Vec<String> {
    let content = match std::fs::read_to_string(abs_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "Warning: could not read {}: {}",
                abs_path.display(),
                e
            );
            return Vec::new();
        }
    };

    let schema = match yaml_schema::parse_schema_file(&content) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "Warning: could not parse {}: {}",
                abs_path.display(),
                e
            );
            return Vec::new();
        }
    };

    let mut names = Vec::new();
    for source in &schema.sources {
        for table in &source.tables {
            names.push(format!("{}.{}", source.name, table.name));
        }
    }
    for model in &schema.models {
        names.push(model.name.clone());
    }
    names
}

/// Process stdin lines and resolve them to model/source names suitable for
/// use as focus models in `filter_graph`.
pub fn resolve_stdin_inputs(
    lines: &[String],
    graph: &LineageGraph,
    resolved_paths: &ResolvedPaths,
    project_dir: &Path,
) -> Vec<String> {
    let mut names = Vec::new();

    let cwd = std::env::current_dir().unwrap_or_default();

    for line in lines {
        match classify_line(line, resolved_paths, &cwd) {
            InputLine::SqlFile(abs_path) => {
                if let Some(label) = resolve_sql_to_label(&abs_path, graph, project_dir) {
                    names.push(label);
                } else {
                    eprintln!(
                        "Warning: no node found for file {}, skipping.",
                        abs_path.display()
                    );
                }
            }
            InputLine::YamlFile(abs_path) => {
                names.extend(expand_yaml_names(&abs_path));
            }
            InputLine::ModelName(name) => {
                names.push(name);
            }
            InputLine::Ignore => {}
        }
    }

    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::types::{NodeData, NodeType};
    use std::fs;

    fn make_resolved_paths(project_dir: &Path) -> ResolvedPaths {
        ResolvedPaths {
            model_paths: vec![project_dir.join("models")],
            seed_paths: vec![project_dir.join("seeds")],
            snapshot_paths: vec![project_dir.join("snapshots")],
            test_paths: vec![project_dir.join("tests")],
            macro_paths: vec![project_dir.join("macros")],
            analysis_paths: vec![project_dir.join("analyses")],
        }
    }

    fn make_node(unique_id: &str, label: &str, node_type: NodeType) -> NodeData {
        NodeData {
            unique_id: unique_id.to_string(),
            label: label.to_string(),
            node_type,
            file_path: None,
            description: None,
            materialization: None,
            tags: vec![],
            columns: vec![],
        }
    }

    // --- classify_line tests ---
    // classify_line uses `cwd` to resolve relative paths to absolute.
    // In tests, we pass the tempdir as cwd so that relative paths resolve correctly.

    #[test]
    fn test_classify_sql_under_models() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_resolved_paths(tmp.path());
        let result = classify_line("models/staging/stg_orders.sql", &paths, tmp.path());
        assert!(matches!(result, InputLine::SqlFile(_)));
    }

    #[test]
    fn test_classify_sql_under_snapshots() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_resolved_paths(tmp.path());
        let result = classify_line("snapshots/snap_orders.sql", &paths, tmp.path());
        assert!(matches!(result, InputLine::SqlFile(_)));
    }

    #[test]
    fn test_classify_sql_under_analyses() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_resolved_paths(tmp.path());
        let result = classify_line("analyses/my_analysis.sql", &paths, tmp.path());
        assert!(matches!(result, InputLine::SqlFile(_)));
    }

    #[test]
    fn test_classify_sql_outside_dbt_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_resolved_paths(tmp.path());
        let result = classify_line("other/script.sql", &paths, tmp.path());
        assert!(matches!(result, InputLine::Ignore));
    }

    #[test]
    fn test_classify_yml_under_models() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_resolved_paths(tmp.path());
        let result = classify_line("models/staging/schema.yml", &paths, tmp.path());
        assert!(matches!(result, InputLine::YamlFile(_)));
    }

    #[test]
    fn test_classify_yaml_under_models() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_resolved_paths(tmp.path());
        let result = classify_line("models/schema.yaml", &paths, tmp.path());
        assert!(matches!(result, InputLine::YamlFile(_)));
    }

    #[test]
    fn test_classify_yml_outside_dbt_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_resolved_paths(tmp.path());
        let result = classify_line(".github/workflows/ci.yml", &paths, tmp.path());
        assert!(matches!(result, InputLine::Ignore));
    }

    #[test]
    fn test_classify_non_dbt_extension_with_separator() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_resolved_paths(tmp.path());
        // Files with path separators and non-dbt extensions are ignored
        assert!(matches!(
            classify_line("seeds/data.csv", &paths, tmp.path()),
            InputLine::Ignore
        ));
        assert!(matches!(
            classify_line("models/model.py", &paths, tmp.path()),
            InputLine::Ignore
        ));
    }

    #[test]
    fn test_classify_non_dbt_extension_without_separator() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_resolved_paths(tmp.path());
        // Files without path separators are treated as model names
        // (the downstream try_resolve_node will handle unknown names)
        assert!(matches!(
            classify_line("README.md", &paths, tmp.path()),
            InputLine::ModelName(ref n) if n == "README.md"
        ));
    }

    #[test]
    fn test_classify_no_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_resolved_paths(tmp.path());
        let result = classify_line("stg_orders", &paths, tmp.path());
        assert!(matches!(result, InputLine::ModelName(ref n) if n == "stg_orders"));
    }

    #[test]
    fn test_classify_source_name() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_resolved_paths(tmp.path());
        // "raw.orders" has no recognized extension (.orders is not .sql/.yml/.yaml)
        let result = classify_line("raw.orders", &paths, tmp.path());
        assert!(matches!(result, InputLine::ModelName(ref n) if n == "raw.orders"));
    }

    // --- is_under_dbt_paths tests ---

    #[test]
    fn test_is_under_dbt_paths_nested() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_resolved_paths(tmp.path());
        let abs = tmp.path().join("models/staging/stg_orders.sql");
        assert!(is_under_dbt_paths(&abs, &paths));
    }

    #[test]
    fn test_is_under_dbt_paths_absolute() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_resolved_paths(tmp.path());
        let abs = tmp.path().join("models/orders.sql");
        assert!(is_under_dbt_paths(&abs, &paths));
    }

    #[test]
    fn test_is_not_under_dbt_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_resolved_paths(tmp.path());
        let abs = tmp.path().join("other/file.sql");
        assert!(!is_under_dbt_paths(&abs, &paths));
    }

    // --- resolve_sql_to_label tests ---

    #[test]
    fn test_resolve_sql_to_label_found() {
        let project_dir = Path::new("/project");
        let mut graph = LineageGraph::new();
        let mut node = make_node("model.stg_orders", "stg_orders", NodeType::Model);
        node.file_path = Some(PathBuf::from("models/staging/stg_orders.sql"));
        graph.add_node(node);

        let abs = Path::new("/project/models/staging/stg_orders.sql");
        let result = resolve_sql_to_label(abs, &graph, project_dir);
        assert_eq!(result, Some("stg_orders".to_string()));
    }

    #[test]
    fn test_resolve_sql_to_label_not_found() {
        let project_dir = Path::new("/project");
        let graph = LineageGraph::new();

        let abs = Path::new("/project/models/nonexistent.sql");
        let result = resolve_sql_to_label(abs, &graph, project_dir);
        assert_eq!(result, None);
    }

    // --- expand_yaml_names tests ---

    #[test]
    fn test_expand_yaml_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let yaml_path = tmp.path().join("schema.yml");
        fs::write(
            &yaml_path,
            r#"
sources:
  - name: raw
    tables:
      - name: orders
      - name: customers
"#,
        )
        .unwrap();

        let names = expand_yaml_names(&yaml_path);
        assert_eq!(names, vec!["raw.orders", "raw.customers"]);
    }

    #[test]
    fn test_expand_yaml_models() {
        let tmp = tempfile::tempdir().unwrap();
        let yaml_path = tmp.path().join("schema.yml");
        fs::write(
            &yaml_path,
            r#"
models:
  - name: stg_orders
  - name: stg_customers
"#,
        )
        .unwrap();

        let names = expand_yaml_names(&yaml_path);
        assert_eq!(names, vec!["stg_orders", "stg_customers"]);
    }

    #[test]
    fn test_expand_yaml_mixed() {
        let tmp = tempfile::tempdir().unwrap();
        let yaml_path = tmp.path().join("schema.yml");
        fs::write(
            &yaml_path,
            r#"
sources:
  - name: raw
    tables:
      - name: orders
models:
  - name: stg_orders
"#,
        )
        .unwrap();

        let names = expand_yaml_names(&yaml_path);
        assert_eq!(names, vec!["raw.orders", "stg_orders"]);
    }

    #[test]
    fn test_expand_yaml_file_not_found() {
        let names = expand_yaml_names(Path::new("/nonexistent/schema.yml"));
        assert!(names.is_empty());
    }

    #[test]
    fn test_expand_yaml_empty_file() {
        let tmp = tempfile::tempdir().unwrap();
        let yaml_path = tmp.path().join("schema.yml");
        fs::write(&yaml_path, "").unwrap();

        let names = expand_yaml_names(&yaml_path);
        assert!(names.is_empty());
    }

    // --- resolve_stdin_inputs integration tests ---
    // resolve_stdin_inputs uses std::env::current_dir() internally.
    // For these tests we use absolute paths (via tempdir) in the input lines
    // to avoid CWD dependency. Alternatively we test classify_line directly
    // with explicit cwd.

    #[test]
    fn test_resolve_stdin_model_name() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_resolved_paths(tmp.path());
        let graph = LineageGraph::new();

        let lines = vec!["stg_orders".to_string()];
        let result = resolve_stdin_inputs(&lines, &graph, &paths, tmp.path());
        assert_eq!(result, vec!["stg_orders"]);
    }

    #[test]
    fn test_resolve_stdin_ignores_non_dbt() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_resolved_paths(tmp.path());
        let graph = LineageGraph::new();

        // Files with path separators and non-dbt extensions are ignored
        let lines = vec![
            "docs/README.md".to_string(),
            "seeds/data.csv".to_string(),
        ];
        let result = resolve_stdin_inputs(&lines, &graph, &paths, tmp.path());
        assert!(result.is_empty());
    }

    // --- classify_line + resolve integration (cwd-aware) ---

    #[test]
    fn test_classify_and_resolve_sql() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_resolved_paths(tmp.path());
        let mut graph = LineageGraph::new();
        let mut node = make_node("model.stg_orders", "stg_orders", NodeType::Model);
        node.file_path = Some(PathBuf::from("models/staging/stg_orders.sql"));
        graph.add_node(node);

        // Simulate: cwd = tmp.path(), stdin line = relative path
        let line = "models/staging/stg_orders.sql";
        match classify_line(line, &paths, tmp.path()) {
            InputLine::SqlFile(abs_path) => {
                let label = resolve_sql_to_label(&abs_path, &graph, tmp.path());
                assert_eq!(label, Some("stg_orders".to_string()));
            }
            other => panic!("Expected SqlFile, got {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn test_classify_and_resolve_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        let models_dir = tmp.path().join("models");
        fs::create_dir_all(&models_dir).unwrap();
        fs::write(
            models_dir.join("schema.yml"),
            "sources:\n  - name: raw\n    tables:\n      - name: orders\n",
        )
        .unwrap();

        let paths = make_resolved_paths(tmp.path());

        let line = "models/schema.yml";
        match classify_line(line, &paths, tmp.path()) {
            InputLine::YamlFile(abs_path) => {
                let names = expand_yaml_names(&abs_path);
                assert_eq!(names, vec!["raw.orders"]);
            }
            other => panic!("Expected YamlFile, got {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn test_classify_and_resolve_mixed() {
        let tmp = tempfile::tempdir().unwrap();
        let models_dir = tmp.path().join("models");
        fs::create_dir_all(models_dir.join("staging")).unwrap();
        fs::write(
            models_dir.join("schema.yml"),
            "sources:\n  - name: raw\n    tables:\n      - name: orders\n",
        )
        .unwrap();

        let paths = make_resolved_paths(tmp.path());
        let mut graph = LineageGraph::new();
        let mut node = make_node("model.stg_orders", "stg_orders", NodeType::Model);
        node.file_path = Some(PathBuf::from("models/staging/stg_orders.sql"));
        graph.add_node(node);

        let inputs = vec![
            "models/staging/stg_orders.sql",
            "models/schema.yml",
            "raw.customers",
            ".github/workflows/ci.yml",
            "docs/README.md",
        ];

        let mut result = Vec::new();
        for line in inputs {
            match classify_line(line, &paths, tmp.path()) {
                InputLine::SqlFile(abs) => {
                    if let Some(label) = resolve_sql_to_label(&abs, &graph, tmp.path()) {
                        result.push(label);
                    }
                }
                InputLine::YamlFile(abs) => {
                    result.extend(expand_yaml_names(&abs));
                }
                InputLine::ModelName(name) => result.push(name),
                InputLine::Ignore => {}
            }
        }
        assert_eq!(result, vec!["stg_orders", "raw.orders", "raw.customers"]);
    }

    #[test]
    fn test_subdir_project_path_resolution() {
        // Simulate: git root = tmp, dbt project in tmp/dbt/
        let tmp = tempfile::tempdir().unwrap();
        let dbt_dir = tmp.path().join("dbt");
        let models_dir = dbt_dir.join("models");
        fs::create_dir_all(&models_dir).unwrap();

        // resolved_paths are absolute under dbt_dir
        let paths = make_resolved_paths(&dbt_dir);

        let mut graph = LineageGraph::new();
        let mut node = make_node("model.stg_orders", "stg_orders", NodeType::Model);
        // file_path stored relative to project_dir (dbt/)
        node.file_path = Some(PathBuf::from("models/stg_orders.sql"));
        graph.add_node(node);

        // stdin line is relative to CWD (git root), so includes "dbt/" prefix
        let line = "dbt/models/stg_orders.sql";
        // cwd = git root (tmp.path())
        match classify_line(line, &paths, tmp.path()) {
            InputLine::SqlFile(abs_path) => {
                // abs_path should be tmp/dbt/models/stg_orders.sql
                // project_dir = dbt_dir = tmp/dbt
                let label = resolve_sql_to_label(&abs_path, &graph, &dbt_dir);
                assert_eq!(label, Some("stg_orders".to_string()));
            }
            other => panic!("Expected SqlFile, got {:?}", std::mem::discriminant(&other)),
        }
    }
}
