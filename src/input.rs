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
        Some(ext) => {
            // Has a non-dbt file extension. If it has a path separator it's
            // clearly a file path → ignore.  Without a separator it could be
            // a dbt source name like "raw.orders" (extension = "orders") or a
            // root-level file like "README.md".  We distinguish them by
            // checking against common file extensions.
            if line.contains('/') || line.contains('\\') {
                InputLine::Ignore
            } else if is_common_file_extension(ext) {
                InputLine::Ignore
            } else {
                InputLine::ModelName(line.to_string())
            }
        }
        None => {
            // No extension at all (e.g. "stg_orders", "Makefile").
            // Lines with a path separator are non-dbt paths → ignore.
            if line.contains('/') || line.contains('\\') {
                InputLine::Ignore
            } else {
                InputLine::ModelName(line.to_string())
            }
        }
    }
}

/// Common file extensions that are NOT dbt source/model names.
/// Used to distinguish root-level files (e.g. "README.md") from dbt source
/// references (e.g. "raw.orders") when there is no path separator.
///
/// Note: this allowlist is inherently incomplete.  In the rare case that a
/// dbt source table name collides with a listed extension (e.g. a table
/// literally named "py" referenced as "raw.py"), the input will be silently
/// ignored.  Use an explicit model name without the source prefix, or pass
/// the schema YAML path instead.
fn is_common_file_extension(ext: &str) -> bool {
    matches!(
        ext,
        "md" | "txt"
            | "py"
            | "csv"
            | "json"
            | "toml"
            | "cfg"
            | "ini"
            | "rst"
            | "lock"
            | "xml"
            | "html"
            | "htm"
            | "js"
            | "ts"
            | "sh"
            | "bat"
            | "rs"
            | "go"
            | "java"
            | "rb"
            | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "swift"
            | "kt"
            | "log"
            | "env"
            | "gitignore"
    )
}

/// Check whether any of the given input strings look like file paths rather
/// than bare model names.  Used to decide whether to load `DbtProject` for
/// path resolution.
pub fn has_path_like_input(inputs: &[String]) -> bool {
    inputs.iter().any(|s| {
        s.contains('/')
            || s.contains('\\')
            || s.ends_with(".sql")
            || s.ends_with(".yml")
            || s.ends_with(".yaml")
    })
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

    let schema = match yaml_schema::parse_schema_file(&content, Some(abs_path)) {
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

/// Process input lines and resolve them to model/source names suitable for
/// use as focus models in `filter_graph`.
///
/// `cwd` is the working directory used to resolve relative file paths (e.g.
/// paths from `git diff --name-only`).  This may differ from `project_dir`
/// when the dbt project lives in a subdirectory of the git repository.
pub fn resolve_stdin_inputs(
    lines: &[String],
    graph: &LineageGraph,
    resolved_paths: &ResolvedPaths,
    project_dir: &Path,
    cwd: &Path,
) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut names = Vec::new();

    for line in lines {
        match classify_line(line, resolved_paths, cwd) {
            InputLine::SqlFile(abs_path) => {
                if let Some(label) = resolve_sql_to_label(&abs_path, graph, project_dir) {
                    if seen.insert(label.clone()) {
                        names.push(label);
                    }
                } else {
                    eprintln!(
                        "Warning: no node found for file {}, skipping.",
                        abs_path.display()
                    );
                }
            }
            InputLine::YamlFile(abs_path) => {
                for name in expand_yaml_names(&abs_path) {
                    if seen.insert(name.clone()) {
                        names.push(name);
                    }
                }
            }
            InputLine::ModelName(name) => {
                if seen.insert(name.clone()) {
                    names.push(name);
                }
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
        // Root-level files with common extensions are ignored
        assert!(matches!(
            classify_line("README.md", &paths, tmp.path()),
            InputLine::Ignore
        ));
        assert!(matches!(
            classify_line("Cargo.toml", &paths, tmp.path()),
            InputLine::Ignore
        ));
        assert!(matches!(
            classify_line("setup.py", &paths, tmp.path()),
            InputLine::Ignore
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

    // --- has_path_like_input tests ---

    #[test]
    fn test_has_path_like_input_with_paths() {
        assert!(has_path_like_input(&["models/foo.sql".into()]));
        assert!(has_path_like_input(&["stg_orders".into(), "models/bar.yml".into()]));
        assert!(has_path_like_input(&["schema.yaml".into()]));
    }

    #[test]
    fn test_has_path_like_input_model_names_only() {
        assert!(!has_path_like_input(&["stg_orders".into()]));
        assert!(!has_path_like_input(&["raw.orders".into(), "customers".into()]));
    }

    // --- resolve_stdin_inputs integration tests ---

    #[test]
    fn test_resolve_stdin_model_name() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_resolved_paths(tmp.path());
        let graph = LineageGraph::new();

        let lines = vec!["stg_orders".to_string()];
        let result = resolve_stdin_inputs(&lines, &graph, &paths, tmp.path(), tmp.path());
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
        let result = resolve_stdin_inputs(&lines, &graph, &paths, tmp.path(), tmp.path());
        assert!(result.is_empty());
    }

    #[test]
    fn test_resolve_stdin_deduplicates() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_resolved_paths(tmp.path());
        let mut graph = LineageGraph::new();
        let mut node = make_node("model.stg_orders", "stg_orders", NodeType::Model);
        node.file_path = Some(PathBuf::from("models/stg_orders.sql"));
        graph.add_node(node);

        // Same model referenced both as file path and model name
        let models_dir = tmp.path().join("models");
        fs::create_dir_all(&models_dir).unwrap();
        let lines = vec![
            "models/stg_orders.sql".to_string(),
            "stg_orders".to_string(),
        ];
        let result = resolve_stdin_inputs(&lines, &graph, &paths, tmp.path(), tmp.path());
        assert_eq!(result, vec!["stg_orders"]);
    }

    #[test]
    fn test_resolve_stdin_ignores_root_files() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = make_resolved_paths(tmp.path());
        let graph = LineageGraph::new();

        // Root-level files with common extensions are ignored (no separator)
        let lines = vec![
            "README.md".to_string(),
            "Cargo.toml".to_string(),
            "stg_orders".to_string(),
        ];
        let result = resolve_stdin_inputs(&lines, &graph, &paths, tmp.path(), tmp.path());
        assert_eq!(result, vec!["stg_orders"]);
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
