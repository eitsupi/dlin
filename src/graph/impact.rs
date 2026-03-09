use std::collections::{HashMap, HashSet, VecDeque};

use petgraph::stable_graph::NodeIndex;
use petgraph::visit::EdgeRef;
use petgraph::Direction;
use serde::Serialize;

use super::types::*;

/// Severity level of impact
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ImpactSeverity {
    Low,
    Medium,
    High,
    Critical,
}

impl ImpactSeverity {
    pub fn label(&self) -> &'static str {
        match self {
            ImpactSeverity::Low => "low",
            ImpactSeverity::Medium => "medium",
            ImpactSeverity::High => "high",
            ImpactSeverity::Critical => "critical",
        }
    }
}

/// Path from source model to an exposure
#[derive(Debug, Clone, Serialize)]
pub struct ExposurePath {
    pub exposure: String,
    pub path: Vec<String>,
}

/// A single impacted node with its severity
#[derive(Debug, Clone, Serialize)]
pub struct ImpactedNode {
    pub unique_id: String,
    pub label: String,
    pub node_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    pub severity: ImpactSeverity,
    /// Number of edges from the source model (also known as "degree" in dbt terminology)
    pub distance: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sql_content: Option<String>,
}

/// Full impact analysis report
#[derive(Debug, Clone, Serialize)]
pub struct ImpactReport {
    pub source_model: String,
    pub overall_severity: ImpactSeverity,
    pub affected_models: usize,
    pub affected_tests: usize,
    pub affected_exposures: usize,
    pub exposure_paths: Vec<ExposurePath>,
    /// True if any exposure had more paths than the enumeration cap
    pub exposure_paths_truncated: bool,
    pub impacted_nodes: Vec<ImpactedNode>,
}

/// Classify the severity of a single node
pub fn classify_severity(node: &NodeData) -> ImpactSeverity {
    match node.node_type {
        NodeType::Exposure => ImpactSeverity::Critical,
        NodeType::Test => ImpactSeverity::Low,
        NodeType::Model => {
            // Check for mart-like indicators
            let is_mart = node
                .materialization
                .as_deref()
                .is_some_and(|m| m == "table" || m == "incremental")
                || node
                    .file_path
                    .as_ref()
                    .is_some_and(|p| p.to_string_lossy().contains("mart"));

            if is_mart {
                return ImpactSeverity::High;
            }

            ImpactSeverity::Medium
        }
        _ => ImpactSeverity::Medium,
    }
}

/// Compute the full impact report for a given model
pub fn compute_impact(graph: &LineageGraph, source_idx: NodeIndex) -> ImpactReport {
    let source_node = &graph[source_idx];
    let source_model = source_node.label.clone();

    // BFS downstream to find all impacted nodes with distances
    let mut visited: HashSet<NodeIndex> = HashSet::new();
    let mut queue: VecDeque<(NodeIndex, usize)> = VecDeque::new();
    visited.insert(source_idx);
    queue.push_back((source_idx, 0));

    let mut impacted_nodes: Vec<ImpactedNode> = Vec::new();
    let mut affected_models = 0usize;
    let mut affected_tests = 0usize;
    let mut affected_exposures = 0usize;
    let mut exposure_indices: Vec<NodeIndex> = Vec::new();

    while let Some((current, distance)) = queue.pop_front() {
        for edge in graph.edges_directed(current, Direction::Outgoing) {
            let neighbor = edge.target();
            if visited.insert(neighbor) {
                let node = &graph[neighbor];
                let severity = classify_severity(node);
                let next_distance = distance + 1;

                match node.node_type {
                    NodeType::Model => affected_models += 1,
                    NodeType::Test => affected_tests += 1,
                    NodeType::Exposure => {
                        affected_exposures += 1;
                        exposure_indices.push(neighbor);
                    }
                    _ => {}
                }

                impacted_nodes.push(ImpactedNode {
                    unique_id: node.unique_id.clone(),
                    label: node.label.clone(),
                    node_type: node.node_type.label().to_string(),
                    file_path: node
                        .file_path
                        .as_ref()
                        .map(|p| p.to_string_lossy().into_owned()),
                    severity,
                    distance: next_distance,
                    sql_content: None,
                });

                queue.push_back((neighbor, next_distance));
            }
        }
    }

    // Find all simple paths to exposures via single DFS (capped per exposure)
    const MAX_PATHS_PER_EXPOSURE: usize = 10;
    let exposure_set: HashSet<NodeIndex> = exposure_indices.iter().copied().collect();
    let mut exposure_paths: Vec<ExposurePath> = Vec::new();
    let mut path_counts: HashMap<NodeIndex, usize> = HashMap::new();

    if !exposure_set.is_empty() {
        let mut stack: Vec<(NodeIndex, Vec<NodeIndex>, HashSet<NodeIndex>)> = vec![(
            source_idx,
            vec![source_idx],
            HashSet::from([source_idx]),
        )];
        while let Some((current, path, path_set)) = stack.pop() {
            // Early termination: stop when all exposures have reached the cap
            if path_counts.len() == exposure_set.len()
                && path_counts.values().all(|&c| c >= MAX_PATHS_PER_EXPOSURE)
            {
                break;
            }
            if exposure_set.contains(&current) {
                let count = path_counts.entry(current).or_insert(0);
                if *count < MAX_PATHS_PER_EXPOSURE {
                    *count += 1;
                    exposure_paths.push(ExposurePath {
                        exposure: graph[current].label.clone(),
                        path: path.iter().map(|&idx| graph[idx].label.clone()).collect(),
                    });
                }
                // Exposures are leaf nodes in dbt; don't traverse beyond them
                continue;
            }
            for edge in graph.edges_directed(current, Direction::Outgoing) {
                let neighbor = edge.target();
                if !path_set.contains(&neighbor) {
                    let mut new_path = path.clone();
                    new_path.push(neighbor);
                    let mut new_set = path_set.clone();
                    new_set.insert(neighbor);
                    stack.push((neighbor, new_path, new_set));
                }
            }
        }
    }
    let exposure_paths_truncated = path_counts
        .values()
        .any(|&count| count >= MAX_PATHS_PER_EXPOSURE);
    exposure_paths.sort_by(|a, b| a.exposure.cmp(&b.exposure).then(a.path.cmp(&b.path)));

    // Sort by severity (descending), then distance
    impacted_nodes.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then(a.distance.cmp(&b.distance))
    });

    let overall_severity = impacted_nodes
        .iter()
        .map(|n| n.severity)
        .max()
        .unwrap_or(ImpactSeverity::Low);

    ImpactReport {
        source_model,
        overall_severity,
        affected_models,
        affected_tests,
        affected_exposures,
        exposure_paths,
        exposure_paths_truncated,
        impacted_nodes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_node(
        unique_id: &str,
        label: &str,
        node_type: NodeType,
        materialization: Option<&str>,
        file_path: Option<&str>,
    ) -> NodeData {
        NodeData {
            unique_id: unique_id.into(),
            label: label.into(),
            node_type,
            file_path: file_path.map(PathBuf::from),
            description: None,
            materialization: materialization.map(|s| s.to_string()),
            tags: vec![],
            columns: vec![],
        }
    }

    fn make_test_graph() -> (LineageGraph, NodeIndex) {
        let mut g = LineageGraph::new();
        let src = g.add_node(make_node(
            "source.raw.orders",
            "raw.orders",
            NodeType::Source,
            None,
            None,
        ));
        let stg = g.add_node(make_node(
            "model.stg_orders",
            "stg_orders",
            NodeType::Model,
            Some("view"),
            Some("models/staging/stg_orders.sql"),
        ));
        let mart = g.add_node(make_node(
            "model.orders",
            "orders",
            NodeType::Model,
            Some("table"),
            Some("models/marts/orders.sql"),
        ));
        let test = g.add_node(make_node(
            "test.orders_positive",
            "orders_positive",
            NodeType::Test,
            None,
            None,
        ));
        let exp = g.add_node(make_node(
            "exposure.dashboard",
            "dashboard",
            NodeType::Exposure,
            None,
            None,
        ));

        g.add_edge(
            src,
            stg,
            EdgeData {
                edge_type: EdgeType::Source,
            },
        );
        g.add_edge(
            stg,
            mart,
            EdgeData {
                edge_type: EdgeType::Ref,
            },
        );
        g.add_edge(
            mart,
            test,
            EdgeData {
                edge_type: EdgeType::Test,
            },
        );
        g.add_edge(
            mart,
            exp,
            EdgeData {
                edge_type: EdgeType::Exposure,
            },
        );

        (g, stg)
    }

    #[test]
    fn test_classify_severity_exposure() {
        let node = make_node("exposure.x", "x", NodeType::Exposure, None, None);
        assert_eq!(classify_severity(&node), ImpactSeverity::Critical);
    }

    #[test]
    fn test_classify_severity_test() {
        let node = make_node("test.x", "x", NodeType::Test, None, None);
        assert_eq!(classify_severity(&node), ImpactSeverity::Low);
    }

    #[test]
    fn test_classify_severity_mart_table() {
        let node = make_node(
            "model.orders",
            "orders",
            NodeType::Model,
            Some("table"),
            None,
        );
        assert_eq!(classify_severity(&node), ImpactSeverity::High);
    }

    #[test]
    fn test_classify_severity_mart_incremental() {
        let node = make_node(
            "model.orders",
            "orders",
            NodeType::Model,
            Some("incremental"),
            None,
        );
        assert_eq!(classify_severity(&node), ImpactSeverity::High);
    }

    #[test]
    fn test_classify_severity_mart_path() {
        let node = make_node(
            "model.orders",
            "orders",
            NodeType::Model,
            None,
            Some("models/marts/orders.sql"),
        );
        assert_eq!(classify_severity(&node), ImpactSeverity::High);
    }

    #[test]
    fn test_classify_severity_staging() {
        let node = make_node(
            "model.stg_orders",
            "stg_orders",
            NodeType::Model,
            Some("view"),
            Some("models/staging/stg_orders.sql"),
        );
        assert_eq!(classify_severity(&node), ImpactSeverity::Medium);
    }

    #[test]
    fn test_compute_impact() {
        let (g, stg) = make_test_graph();
        let report = compute_impact(&g, stg);

        assert_eq!(report.source_model, "stg_orders");
        assert_eq!(report.affected_models, 1); // orders
        assert_eq!(report.affected_tests, 1); // orders_positive
        assert_eq!(report.affected_exposures, 1); // dashboard
        assert_eq!(report.overall_severity, ImpactSeverity::Critical);
        assert_eq!(report.impacted_nodes.len(), 3);

        // Exposure path: stg_orders -> orders -> dashboard
        assert_eq!(report.exposure_paths.len(), 1);
        assert_eq!(report.exposure_paths[0].exposure, "dashboard");
        assert_eq!(
            report.exposure_paths[0].path,
            vec!["stg_orders", "orders", "dashboard"]
        );
    }

    #[test]
    fn test_compute_impact_leaf_node() {
        let (g, _) = make_test_graph();
        let exp = g
            .node_indices()
            .find(|&i| g[i].label == "dashboard")
            .unwrap();
        let report = compute_impact(&g, exp);

        assert_eq!(report.source_model, "dashboard");
        assert_eq!(report.affected_models, 0);
        assert_eq!(report.affected_tests, 0);
        assert_eq!(report.affected_exposures, 0);
        assert!(report.impacted_nodes.is_empty());
        assert!(report.exposure_paths.is_empty());
    }

    #[test]
    fn test_impact_severity_ordering() {
        assert!(ImpactSeverity::Low < ImpactSeverity::Medium);
        assert!(ImpactSeverity::Medium < ImpactSeverity::High);
        assert!(ImpactSeverity::High < ImpactSeverity::Critical);
    }

    #[test]
    fn test_impact_isolated_node() {
        let mut g = LineageGraph::new();
        let n = g.add_node(make_node("model.x", "x", NodeType::Model, None, None));
        let report = compute_impact(&g, n);
        assert_eq!(report.affected_models, 0);
        assert_eq!(report.affected_tests, 0);
        assert_eq!(report.affected_exposures, 0);
        assert!(report.impacted_nodes.is_empty());
    }

    #[test]
    fn test_exposure_paths_multiple_exposures() {
        // Diamond graph: src -> A -> exp1, src -> B -> exp2
        let mut g = LineageGraph::new();
        let src = g.add_node(make_node(
            "model.src",
            "src",
            NodeType::Model,
            Some("view"),
            None,
        ));
        let a = g.add_node(make_node(
            "model.a",
            "a",
            NodeType::Model,
            Some("view"),
            None,
        ));
        let b = g.add_node(make_node(
            "model.b",
            "b",
            NodeType::Model,
            Some("view"),
            None,
        ));
        let exp1 = g.add_node(make_node(
            "exposure.dashboard",
            "dashboard",
            NodeType::Exposure,
            None,
            None,
        ));
        let exp2 = g.add_node(make_node(
            "exposure.report",
            "report",
            NodeType::Exposure,
            None,
            None,
        ));

        g.add_edge(src, a, EdgeData { edge_type: EdgeType::Ref });
        g.add_edge(src, b, EdgeData { edge_type: EdgeType::Ref });
        g.add_edge(a, exp1, EdgeData { edge_type: EdgeType::Exposure });
        g.add_edge(b, exp2, EdgeData { edge_type: EdgeType::Exposure });

        let report = compute_impact(&g, src);
        assert_eq!(report.affected_exposures, 2);
        assert_eq!(report.exposure_paths.len(), 2);

        // Sorted by exposure name
        assert_eq!(report.exposure_paths[0].exposure, "dashboard");
        assert_eq!(report.exposure_paths[0].path, vec!["src", "a", "dashboard"]);
        assert_eq!(report.exposure_paths[1].exposure, "report");
        assert_eq!(report.exposure_paths[1].path, vec!["src", "b", "report"]);
    }

    #[test]
    fn test_exposure_paths_diamond_convergent() {
        // Diamond: src -> A -> C -> exp, src -> B -> C -> exp
        // BFS finds shortest path (through whichever of A/B is visited first)
        let mut g = LineageGraph::new();
        let src = g.add_node(make_node(
            "model.src",
            "src",
            NodeType::Model,
            Some("view"),
            None,
        ));
        let a = g.add_node(make_node(
            "model.a",
            "a",
            NodeType::Model,
            Some("view"),
            None,
        ));
        let b = g.add_node(make_node(
            "model.b",
            "b",
            NodeType::Model,
            Some("view"),
            None,
        ));
        let c = g.add_node(make_node(
            "model.c",
            "c",
            NodeType::Model,
            Some("table"),
            None,
        ));
        let exp = g.add_node(make_node(
            "exposure.dashboard",
            "dashboard",
            NodeType::Exposure,
            None,
            None,
        ));

        g.add_edge(src, a, EdgeData { edge_type: EdgeType::Ref });
        g.add_edge(src, b, EdgeData { edge_type: EdgeType::Ref });
        g.add_edge(a, c, EdgeData { edge_type: EdgeType::Ref });
        g.add_edge(b, c, EdgeData { edge_type: EdgeType::Ref });
        g.add_edge(c, exp, EdgeData { edge_type: EdgeType::Exposure });

        let report = compute_impact(&g, src);
        assert_eq!(report.affected_exposures, 1);
        // Both paths should be found: src->a->c->dashboard and src->b->c->dashboard
        assert_eq!(report.exposure_paths.len(), 2);
        assert_eq!(
            report.exposure_paths[0].path,
            vec!["src", "a", "c", "dashboard"]
        );
        assert_eq!(
            report.exposure_paths[1].path,
            vec!["src", "b", "c", "dashboard"]
        );
    }

    #[test]
    fn test_classify_severity_source_seed_snapshot() {
        // Covers the wildcard arm (line 76): Source, Seed, Snapshot → Medium
        let source = make_node("source.raw.o", "raw.o", NodeType::Source, None, None);
        assert_eq!(classify_severity(&source), ImpactSeverity::Medium);

        let seed = make_node("seed.countries", "countries", NodeType::Seed, None, None);
        assert_eq!(classify_severity(&seed), ImpactSeverity::Medium);

        let snap = make_node("snapshot.snap", "snap", NodeType::Snapshot, None, None);
        assert_eq!(classify_severity(&snap), ImpactSeverity::Medium);
    }
}
