use super::*;
use crate::graph::dependency::{InputFailure, InputKind, InputLocation, InputStage, ProjectGap};
use crate::parser::discovery::{SkippedInput, discover_files};
use crate::parser::project::DbtProject;
use std::collections::HashMap;

/// Write a project, discover it as the CLI does, and build it.
fn build_project(files: &[(&str, &[u8])]) -> (tempfile::TempDir, GraphBuild) {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("dbt_project.yml"), "name: p\n").unwrap();
    for (path, content) in files {
        let path = tmp.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    let project = DbtProject::load(tmp.path()).unwrap();
    let discovered = discover_files(&project.resolve_paths(tmp.path())).unwrap();
    let build = build_graph_report(tmp.path(), &discovered, &HashMap::new()).unwrap();
    (tmp, build)
}

fn skipped(path: &str, kind: InputKind, stage: InputStage, failure: InputFailure) -> ProjectGap {
    ProjectGap::InputSkipped {
        location: InputLocation {
            path: path.to_string(),
            line: None,
        },
        kind,
        stage,
        failure,
    }
}

fn has_edge(graph: &LineageGraph, from: &str, to: &str) -> bool {
    let find = |id: &str| {
        graph
            .node_indices()
            .find(|&idx| graph[idx].unique_id == id)
            .unwrap_or_else(|| panic!("missing node {id}"))
    };
    graph.contains_edge(find(from), find(to))
}

#[test]
fn complete_project_has_no_gaps() {
    let (_tmp, build) = build_project(&[
        ("models/a.sql", b"select 1"),
        ("models/b.sql", b"select * from {{ ref('a') }}"),
        ("models/schema.yml", b"version: 2\nmodels:\n  - name: a\n"),
        ("macros/m.sql", b"{% macro m() %}1{% endmacro %}"),
    ]);
    assert!(build.project_gaps.is_empty(), "{:?}", build.project_gaps);
}

#[test]
fn unparseable_yaml_is_a_gap_and_other_yaml_is_still_used() {
    let (_tmp, build) = build_project(&[
        ("models/a.sql", b"select 1"),
        ("models/broken.yml", b"models: [unclosed\n"),
        (
            "models/schema.yml",
            b"version: 2\nsources:\n  - name: raw\n    tables:\n      - name: t\n",
        ),
    ]);
    assert_eq!(
        build.project_gaps,
        vec![skipped(
            "models/broken.yml",
            InputKind::Yaml,
            InputStage::Parse,
            InputFailure::ParseError
        )]
    );
    assert!(
        build
            .graph
            .node_indices()
            .any(|idx| build.graph[idx].unique_id == "source.raw.t")
    );
}

#[test]
fn yaml_with_unexpected_structure_is_a_gap() {
    let (_tmp, build) = build_project(&[
        ("models/a.sql", b"select 1"),
        ("models/schema.yml", b"version: 2\nmodels: 3\n"),
    ]);
    assert_eq!(
        build.project_gaps,
        vec![skipped(
            "models/schema.yml",
            InputKind::Yaml,
            InputStage::Parse,
            InputFailure::ParseError
        )]
    );
}

#[test]
fn unparseable_macro_is_a_gap_and_valid_macros_are_still_expanded() {
    let (_tmp, build) = build_project(&[
        ("models/a.sql", b"select 1"),
        ("models/b.sql", b"select * from {{ wrap('a') }}"),
        (
            "macros/good.sql",
            b"{% macro wrap(name) %}{{ ref(name) }}{% endmacro %}",
        ),
        ("macros/bad.sql", b"{% macro broken( %}"),
    ]);
    assert_eq!(
        build.project_gaps,
        vec![skipped(
            "macros/bad.sql",
            InputKind::Macro,
            InputStage::Parse,
            InputFailure::ParseError
        )]
    );
    assert!(has_edge(&build.graph, "model.a", "model.b"));
}

#[test]
fn unreadable_macro_is_a_gap() {
    let (_tmp, build) = build_project(&[
        ("models/a.sql", b"select 1"),
        ("macros/latin1.sql", b"{% macro m() %}\xff{% endmacro %}"),
    ]);
    assert_eq!(
        build.project_gaps,
        vec![skipped(
            "macros/latin1.sql",
            InputKind::Macro,
            InputStage::Read,
            InputFailure::InvalidUtf8
        )]
    );
}

#[test]
fn discovery_gaps_are_reported_relative_sorted_and_deduplicated() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = crate::input::normalize_path(tmp.path());
    let outside = project_dir.parent().unwrap().join("shared/models");
    let inside = project_dir.join("models/locked");
    let entry = |path: &std::path::Path| SkippedInput {
        path: path.to_path_buf(),
        kind: InputKind::Directory,
        stage: InputStage::Enumerate,
        failure: InputFailure::PermissionDenied,
    };
    let files = DiscoveredFiles {
        skipped_inputs: vec![entry(&outside), entry(&inside), entry(&inside)],
        ..Default::default()
    };
    let build = build_graph_report(&project_dir, &files, &HashMap::new()).unwrap();
    let paths: Vec<_> = build
        .project_gaps
        .iter()
        .map(|gap| match gap {
            ProjectGap::InputSkipped { location, .. } => location.path.as_str(),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(paths, vec!["../shared/models", "models/locked"]);
}

#[test]
fn unreadable_yaml_is_still_fatal() {
    let tmp = tempfile::tempdir().unwrap();
    let files = DiscoveredFiles {
        yaml_files: vec![tmp.path().join("models/missing.yml")],
        ..Default::default()
    };
    assert!(build_graph_report(tmp.path(), &files, &HashMap::new()).is_err());
}

#[test]
fn unreadable_model_sql_is_still_fatal() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("models/latin1.sql");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"select '\xff'").unwrap();
    let files = DiscoveredFiles {
        model_sql_files: vec![path],
        ..Default::default()
    };
    assert!(build_graph_report(tmp.path(), &files, &HashMap::new()).is_err());
}

#[test]
fn macro_removed_after_discovery_is_a_gap() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = crate::input::normalize_path(tmp.path());
    let files = DiscoveredFiles {
        macro_sql_files: vec![project_dir.join("macros/gone.sql")],
        ..Default::default()
    };
    let build = build_graph_report(&project_dir, &files, &HashMap::new()).unwrap();
    assert_eq!(
        build.project_gaps,
        vec![skipped(
            "macros/gone.sql",
            InputKind::Macro,
            InputStage::Read,
            InputFailure::NotFound
        )]
    );
}

#[test]
fn macro_exclusions_are_attributed_to_the_right_file_after_a_read_failure() {
    let (_tmp, build) = build_project(&[
        ("models/a.sql", b"select 1"),
        ("macros/a_unreadable.sql", b"\xff"),
        ("macros/b_broken.sql", b"{% macro broken( %}"),
        ("macros/c_good.sql", b"{% macro good() %}1{% endmacro %}"),
    ]);
    assert_eq!(
        build.project_gaps,
        vec![
            skipped(
                "macros/a_unreadable.sql",
                InputKind::Macro,
                InputStage::Read,
                InputFailure::InvalidUtf8
            ),
            skipped(
                "macros/b_broken.sql",
                InputKind::Macro,
                InputStage::Parse,
                InputFailure::ParseError
            ),
        ]
    );
}

#[test]
fn macro_library_reports_unreadable_and_unparseable_macros() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("macros");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("good.sql"), "{% macro good() %}1{% endmacro %}").unwrap();
    fs::write(dir.join("bad.sql"), "{% macro broken( %}").unwrap();
    fs::write(dir.join("latin1.sql"), b"\xff").unwrap();
    let files = DiscoveredFiles {
        macro_sql_files: vec![
            dir.join("good.sql"),
            dir.join("bad.sql"),
            dir.join("latin1.sql"),
        ],
        ..Default::default()
    };
    let mut skipped = Vec::new();
    let library = super::super::super::load_macro_library(&files, &mut skipped);
    assert!(library.get("good").is_some());
    let found: Vec<_> = skipped
        .iter()
        .map(|s| {
            (
                s.path.file_name().unwrap().to_str().unwrap(),
                s.stage,
                s.failure,
            )
        })
        .collect();
    assert_eq!(
        found,
        vec![
            ("latin1.sql", InputStage::Read, InputFailure::InvalidUtf8),
            ("bad.sql", InputStage::Parse, InputFailure::ParseError),
        ]
    );
}
