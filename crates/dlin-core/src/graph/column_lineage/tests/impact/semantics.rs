use super::*;
use crate::parser::manifest::{
    DependsOn, ManifestColumn, ManifestConfig, ManifestNode, ManifestSource,
};

fn model(
    unique_id: &str,
    name: &str,
    dependencies: &[&str],
    sql: &str,
    columns: &[&str],
) -> ManifestNode {
    ManifestNode {
        unique_id: unique_id.to_string(),
        name: name.to_string(),
        alias: None,
        resource_type: "model".to_string(),
        depends_on: DependsOn {
            nodes: dependencies.iter().map(|id| (*id).to_string()).collect(),
        },
        config: ManifestConfig::default(),
        description: None,
        path: None,
        original_file_path: None,
        columns: columns
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    ManifestColumn {
                        name: (*name).to_string(),
                        quote: None,
                    },
                )
            })
            .collect(),
        compiled_code: Some(sql.to_string()),
        database: None,
        schema: None,
    }
}

fn impact(
    manifest: &Manifest,
    model: &str,
    column: &str,
    scope: ImpactScope,
) -> ColumnImpactReport {
    compute_column_impact_with_scope(
        manifest,
        model,
        column,
        scope,
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    )
}

#[test]
fn row_only_dependency_is_complete_for_values_and_not_collected_for_rows() {
    let source = "model.proj.source";
    let filtered = "model.proj.filtered";
    let downstream = "model.proj.downstream";
    let mut nodes = HashMap::new();
    nodes.insert(
        source.to_string(),
        model(source, "source", &[], "SELECT x, y FROM raw_s", &["x", "y"]),
    );
    nodes.insert(
        filtered.to_string(),
        model(
            filtered,
            "filtered",
            &[source],
            "SELECT s.x FROM source s WHERE s.y > 0",
            &["x"],
        ),
    );
    nodes.insert(
        downstream.to_string(),
        model(
            downstream,
            "downstream",
            &[filtered],
            "SELECT x FROM filtered",
            &["x"],
        ),
    );
    let manifest = Manifest {
        nodes,
        ..Default::default()
    };

    let values = impact(&manifest, source, "y", ImpactScope::ValueOnly);
    assert!(values.confirmed.is_empty());
    assert!(values.possible.is_empty());
    assert!(values.proves_no_impact());

    let all = impact(&manifest, source, "y", ImpactScope::ValueAndRow);
    assert!(all.confirmed.is_empty());
    assert!(all.possible.is_empty());
    assert!(!all.proves_no_impact());
    assert!(all.gaps().iter().any(|(kind, gap)| {
        *kind == DependencyKind::Row && matches!(gap, TraceGap::NotCollected { .. })
    }));
}

#[test]
fn ambiguous_model_relation_creates_possible_paths_through_known_downstream_edges() {
    let first = "model.proj.base_a";
    let second = "model.proj.base_b";
    let ambiguous = "model.proj.ambiguous_reader";
    let downstream = "model.proj.downstream";
    let mut first_node = model(first, "base", &[], "SELECT x FROM raw_a", &["x"]);
    first_node.alias = Some("shared_base".to_string());
    let mut second_node = model(second, "base", &[], "SELECT x FROM raw_b", &["x"]);
    second_node.alias = Some("shared_base".to_string());
    let mut nodes = HashMap::new();
    nodes.insert(first.to_string(), first_node);
    nodes.insert(second.to_string(), second_node);
    nodes.insert(
        ambiguous.to_string(),
        model(
            ambiguous,
            "ambiguous_reader",
            &[first, second],
            "SELECT x FROM shared_base",
            &["x"],
        ),
    );
    nodes.insert(
        downstream.to_string(),
        model(
            downstream,
            "downstream",
            &[ambiguous],
            "SELECT x FROM ambiguous_reader",
            &["x"],
        ),
    );
    let manifest = Manifest {
        nodes,
        ..Default::default()
    };

    let report = impact(&manifest, first, "x", ImpactScope::ValueOnly);
    assert!(report.confirmed.is_empty(), "{:?}", report.confirmed);
    assert!(
        report
            .possible
            .iter()
            .any(|path| path.target.model == "ambiguous_reader"),
        "{:?}",
        report.possible
    );
    assert!(
        report
            .possible
            .iter()
            .any(|path| path.target.model == "downstream"),
        "possible paths must continue through later known edges: {:?}",
        report.possible
    );
    assert!(!report.proves_no_impact());
}

#[test]
fn one_output_keeps_a_confirmed_and_an_ambiguous_candidate_path() {
    let root = "model.proj.root";
    let other = "model.proj.other";
    let child = "model.proj.child";
    let mut nodes = HashMap::new();
    nodes.insert(
        root.to_string(),
        model(root, "root", &[], "SELECT x FROM raw", &["x"]),
    );
    nodes.insert(
        other.to_string(),
        model(other, "other", &[], "SELECT x FROM raw_other", &["x"]),
    );
    nodes.insert(
        child.to_string(),
        model(
            child,
            "child",
            &[root, other],
            "SELECT root.x + x AS z FROM root CROSS JOIN other",
            &["z"],
        ),
    );
    let manifest = Manifest {
        nodes,
        ..Default::default()
    };

    let report = impact(&manifest, root, "x", ImpactScope::ValueOnly);
    assert!(
        report
            .confirmed
            .iter()
            .any(|path| { path.target.model == "child" && path.target.column == "z" }),
        "known root.x contribution should remain confirmed: {report:?}"
    );
    assert!(
        report
            .possible
            .iter()
            .any(|path| { path.target.model == "child" && path.target.column == "z" }),
        "the unqualified candidate path should remain possible: {report:?}"
    );
    assert!(report.gaps().iter().any(|(_, gap)| {
        matches!(gap, TraceGap::UnresolvedLineage { at, .. } if at.model == "child")
    }));
}

#[test]
fn duplicate_input_slots_create_a_possible_dependent_path() {
    let source = "model.proj.duplicate_source";
    let child = "model.proj.child";
    let mut nodes = HashMap::new();
    nodes.insert(
        source.to_string(),
        model(
            source,
            "duplicate_source",
            &[],
            "SELECT x, x FROM raw",
            &["x", "x"],
        ),
    );
    nodes.insert(
        child.to_string(),
        model(
            child,
            "child",
            &[source],
            "SELECT x FROM duplicate_source",
            &["x"],
        ),
    );
    let manifest = Manifest {
        nodes,
        ..Default::default()
    };

    let report = impact(&manifest, source, "x", ImpactScope::ValueOnly);
    assert!(report.input.is_none());
    assert!(
        report
            .possible
            .iter()
            .any(|path| path.target.model == "child")
    );
    assert!(report.gaps().iter().any(|(_, gap)| {
        matches!(gap, TraceGap::UnidentifiedInput { candidates, .. } if candidates.len() == 2)
    }));
}

#[test]
fn model_and_source_sharing_a_relation_create_a_possible_path_and_gap() {
    let root = "model.proj.root";
    let source_id = "source.proj.raw.shared";
    let child = "model.proj.child";
    let mut nodes = HashMap::new();
    nodes.insert(
        root.to_string(),
        model(root, "shared", &[], "SELECT x FROM raw", &["x"]),
    );
    nodes.insert(
        child.to_string(),
        model(
            child,
            "child",
            &[root, source_id],
            "SELECT x FROM shared",
            &["x"],
        ),
    );
    let mut source_columns = indexmap::IndexMap::new();
    source_columns.insert(
        "x".to_string(),
        ManifestColumn {
            name: "x".to_string(),
            quote: None,
        },
    );
    let mut sources = HashMap::new();
    sources.insert(
        source_id.to_string(),
        ManifestSource {
            unique_id: source_id.to_string(),
            name: "shared".to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            original_file_path: None,
            columns: source_columns,
            database: None,
            schema: None,
            identifier: Some("shared".to_string()),
        },
    );
    let manifest = Manifest {
        nodes,
        sources,
        ..Default::default()
    };

    let report = impact(&manifest, root, "x", ImpactScope::ValueOnly);
    assert!(
        report
            .possible
            .iter()
            .any(|path| path.target.model == "child")
    );
    assert!(report.gaps().iter().any(|(_, gap)| {
        matches!(gap, TraceGap::AmbiguousModel { at, candidates } if at.model == "child" && candidates.contains(&root.to_string()))
    }));
    assert!(!report.proves_no_impact());
}

#[test]
fn diamond_branch_reachable_from_the_original_input_keeps_its_open_gap() {
    let root = "model.proj.root";
    let left = "model.proj.left";
    let right = "model.proj.right";
    let join = "model.proj.join";
    let mut nodes = HashMap::new();
    nodes.insert(
        root.to_string(),
        model(root, "root", &[], "SELECT x FROM raw", &["x"]),
    );
    nodes.insert(
        left.to_string(),
        model(left, "left", &[root], "SELECT x FROM root", &["x"]),
    );
    nodes.insert(
        right.to_string(),
        model(
            right,
            "right",
            &[root],
            "SELECT root.x, raw.unknown.* FROM root CROSS JOIN raw.unknown",
            &["x"],
        ),
    );
    nodes.insert(
        join.to_string(),
        model(
            join,
            "join",
            &[left, right],
            "SELECT l.x AS x, r.* FROM left l CROSS JOIN right r",
            &["x"],
        ),
    );
    let manifest = Manifest {
        nodes,
        ..Default::default()
    };

    let report = impact(&manifest, root, "x", ImpactScope::ValueOnly);
    assert!(
        report
            .confirmed
            .iter()
            .any(|path| path.target.model == "join")
    );
    assert!(
        report.gaps().iter().any(|(_, gap)| {
            matches!(gap, TraceGap::OpenOutput { model, .. } if model == "join")
        }),
        "the other root-reachable branch must keep the gap: {report:?}"
    );
}

#[test]
fn unknown_partial_candidate_keeps_gap_without_inventing_possible_targets() {
    let root = "model.proj.root";
    let child = "model.proj.child";
    let mut nodes = HashMap::new();
    nodes.insert(
        root.to_string(),
        model(root, "root", &[], "SELECT x FROM raw", &["x"]),
    );
    nodes.insert(
        child.to_string(),
        model(
            child,
            "child",
            &[root],
            "WITH unknown AS (SELECT * FROM raw.unknown) \
             SELECT root.x + unknown.x AS total FROM root CROSS JOIN unknown",
            &["total"],
        ),
    );
    let manifest = Manifest {
        nodes,
        ..Default::default()
    };

    let report = impact(&manifest, root, "x", ImpactScope::ValueOnly);
    assert!(
        report
            .confirmed
            .iter()
            .any(|path| path.target.model == "child")
    );
    assert!(
        report.possible.is_empty(),
        "unknown candidates aren't concrete: {report:?}"
    );
    assert!(report.gaps().iter().any(|(_, gap)| {
        matches!(gap, TraceGap::UnresolvedLineage { at, .. } if at.model == "child")
    }));
}

fn open_relation_manifest(declare_relation: bool) -> Manifest {
    let root = "model.proj.root";
    let child = "model.proj.child";
    let mut nodes = HashMap::new();
    nodes.insert(
        root.to_string(),
        model(root, "root", &[], "SELECT x FROM raw", &["x"]),
    );
    let dependencies = if declare_relation {
        vec![root, "source.proj.raw.other"]
    } else {
        vec![root]
    };
    nodes.insert(
        child.to_string(),
        model(
            child,
            "child",
            &dependencies,
            "SELECT root.x, raw.other.* FROM root CROSS JOIN raw.other",
            &["x"],
        ),
    );
    let mut sources = HashMap::new();
    if declare_relation {
        sources.insert(
            "source.proj.raw.other".to_string(),
            ManifestSource {
                unique_id: "source.proj.raw.other".to_string(),
                name: "other".to_string(),
                source_name: "raw".to_string(),
                resource_type: "source".to_string(),
                description: None,
                path: None,
                original_file_path: None,
                columns: Default::default(),
                database: None,
                schema: None,
                identifier: None,
            },
        );
    }
    Manifest {
        nodes,
        sources,
        ..Default::default()
    }
}

#[test]
fn open_output_is_excluded_only_for_a_declared_unrelated_relation() {
    let known_external = impact(
        &open_relation_manifest(true),
        "root",
        "x",
        ImpactScope::ValueOnly,
    );
    assert!(!known_external.proves_no_impact());
    assert!(
        !known_external.gaps().iter().any(|(_, gap)| {
            matches!(gap, TraceGap::OpenOutput { model, .. } if model == "child")
        }),
        "declared unrelated star should not create an OpenOutput gap: {known_external:?}"
    );

    let unknown = impact(
        &open_relation_manifest(false),
        "root",
        "x",
        ImpactScope::ValueOnly,
    );
    assert!(!unknown.proves_no_impact());
    assert!(
        unknown.gaps().iter().any(|(_, gap)| {
            matches!(gap, TraceGap::OpenOutput { model, .. } if model == "child")
        })
    );
}

#[test]
fn open_relation_matching_an_unrelated_global_model_stays_unknown_without_dependency() {
    let root = "model.proj.root";
    let child = "model.proj.child";
    let unrelated = "model.proj.unrelated";
    let mut nodes = HashMap::new();
    nodes.insert(
        root.to_string(),
        model(root, "root", &[], "SELECT x FROM raw", &["x"]),
    );
    nodes.insert(
        child.to_string(),
        model(
            child,
            "child",
            &[root],
            "SELECT root.x, shared.* FROM root CROSS JOIN shared",
            &["x"],
        ),
    );
    nodes.insert(
        unrelated.to_string(),
        model(
            unrelated,
            "unrelated",
            &[],
            "SELECT y FROM raw_other",
            &["y"],
        ),
    );
    nodes.get_mut(unrelated).unwrap().alias = Some("shared".to_string());
    let manifest = Manifest {
        nodes,
        ..Default::default()
    };

    let report = impact(&manifest, root, "x", ImpactScope::ValueOnly);
    assert!(
        report.gaps().iter().any(|(_, gap)| {
            matches!(gap, TraceGap::OpenOutput { model, .. } if model == "child")
        }),
        "a global name match does not prove what this SQL relation refers to: {report:?}"
    );
}

#[test]
fn partial_lineage_keeps_known_paths_and_excludes_only_declared_unrelated_remainder() {
    let source_model = "model.proj.source_model";
    let partial_model = "model.proj.partial_model";
    let downstream = "model.proj.downstream";
    let fees_source = "source.proj.raw.fees";
    let mut nodes = HashMap::new();
    nodes.insert(
        source_model.to_string(),
        model(
            source_model,
            "source_model",
            &[],
            "SELECT x FROM raw.source_table",
            &["x"],
        ),
    );
    nodes.insert(
        partial_model.to_string(),
        model(
            partial_model,
            "partial_model",
            &[source_model, fees_source],
            "WITH unknown AS (SELECT * FROM raw.fees) \
             SELECT source.x + unknown.fee AS total \
             FROM source_model source CROSS JOIN unknown",
            &["total"],
        ),
    );
    nodes.insert(
        downstream.to_string(),
        model(
            downstream,
            "downstream",
            &[partial_model],
            "SELECT total FROM partial_model",
            &["total"],
        ),
    );
    let mut sources = HashMap::new();
    sources.insert(
        fees_source.to_string(),
        ManifestSource {
            unique_id: fees_source.to_string(),
            name: "fees".to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            original_file_path: None,
            columns: Default::default(),
            database: None,
            schema: None,
            identifier: None,
        },
    );
    let manifest = Manifest {
        nodes,
        sources,
        ..Default::default()
    };

    let report = impact(&manifest, source_model, "x", ImpactScope::ValueOnly);
    assert!(
        report
            .confirmed
            .iter()
            .any(|path| { path.target.model == "partial_model" && path.target.column == "total" })
    );
    assert!(
        report
            .confirmed
            .iter()
            .any(|path| { path.target.model == "downstream" && path.target.column == "total" })
    );
    assert!(report.possible.is_empty());
    assert!(!report.proves_no_impact());
    assert!(!report.gaps().iter().any(|(_, gap)| {
        matches!(gap, TraceGap::UnresolvedLineage { at, .. } if at.model == "partial_model" || at.model == "downstream")
    }), "declared unrelated remainder should not make this impact incomplete: {report:?}");
}

#[test]
fn unsupported_snapshot_dependency_is_an_incomplete_impact_boundary() {
    let root = "model.proj.root";
    let snapshot = "snapshot.proj.snap";
    let mut nodes = HashMap::new();
    nodes.insert(
        root.to_string(),
        model(root, "root", &[], "SELECT x FROM raw", &["x"]),
    );
    let mut snap = model(snapshot, "snap", &[root], "SELECT x FROM root", &["x"]);
    snap.resource_type = "snapshot".to_string();
    nodes.insert(snapshot.to_string(), snap);
    let manifest = Manifest {
        nodes,
        ..Default::default()
    };

    let report = impact(&manifest, root, "x", ImpactScope::ValueOnly);
    assert!(!report.proves_no_impact());
    assert!(report.gaps().iter().any(|(_, gap)| {
        matches!(
            gap,
            TraceGap::AnalysisFailed {
                unique_id,
                error: ColumnLineageErrorKind::UnsupportedMaterialization,
                ..
            } if unique_id == snapshot
        )
    }));
}

#[test]
fn missing_downstream_definition_is_an_incomplete_impact_boundary() {
    let root = "model.proj.root";
    let missing = "model.proj.missing";
    let mut nodes = HashMap::new();
    nodes.insert(
        root.to_string(),
        model(root, "root", &[], "SELECT x FROM raw", &["x"]),
    );
    let mut child = model(missing, "missing", &[root], "SELECT x FROM root", &["x"]);
    child.compiled_code = None;
    nodes.insert(missing.to_string(), child);
    let manifest = Manifest {
        nodes,
        ..Default::default()
    };

    let report = impact(&manifest, root, "x", ImpactScope::ValueOnly);
    assert!(!report.proves_no_impact());
    assert!(report.gaps().iter().any(|(_, gap)| {
        matches!(
            gap,
            TraceGap::AnalysisFailed {
                unique_id,
                error: ColumnLineageErrorKind::NoCompiledCode,
                ..
            } if unique_id == missing
        )
    }));
}

#[test]
fn revisiting_an_output_on_an_impact_path_is_reported_as_a_cycle() {
    let first = "model.proj.first";
    let second = "model.proj.second";
    let mut nodes = HashMap::new();
    nodes.insert(
        first.to_string(),
        model(first, "first", &[second], "SELECT x FROM second", &["x"]),
    );
    nodes.insert(
        second.to_string(),
        model(second, "second", &[first], "SELECT x FROM first", &["x"]),
    );
    let manifest = Manifest {
        nodes,
        ..Default::default()
    };

    let report = impact(&manifest, first, "x", ImpactScope::ValueOnly);
    assert!(
        report
            .gaps()
            .iter()
            .any(|(_, gap)| matches!(gap, TraceGap::Cycle { .. }))
    );
    assert!(!report.proves_no_impact());
}
