use super::*;

/// Build and connect semantic layer nodes (semantic_models, metrics, saved_queries).
///
/// Each definition is paired with the relative path of the YAML file it came from,
/// which is stored on the node for debuggability (consistent with other YAML-derived nodes).
///
/// Pass ordering:
///   1. Register all semantic_model nodes (enables forward references between metrics).
///   2. Build measure → semantic_model_name map.
///   3. Add model → semantic_model edges via `model: ref('...')`.
///   4. Register all metric nodes.
///   5. Add semantic_model → metric edges (Simple metrics via measure lookup).
///   6. Add metric → metric edges (Ratio/Derived/Conversion/Cumulative).
///   7. Register all saved_query nodes and add metric → saved_query edges.
pub(super) fn process_semantic_layer(
    gb: &mut GraphBuilder,
    semantic_models: &[(SemanticModelDefinition, PathBuf)],
    metrics: &[(MetricDefinition, PathBuf)],
    saved_queries: &[(SavedQueryDefinition, PathBuf)],
) {
    // Pass 1: register semantic_model nodes
    for (sm, yaml_path) in semantic_models {
        let unique_id = format!("semantic_model.{}", sm.name);
        if gb.node_map.contains_key(&unique_id) {
            continue;
        }
        gb.add_node(NodeData {
            unique_id,
            label: sm.label.as_deref().unwrap_or(&sm.name).to_string(),
            node_type: NodeType::SemanticModel,
            file_path: Some(yaml_path.clone()),
            description: sm.description.clone(),
            materialization: None,
            tags: vec![],
            columns: vec![],
            exposure: None,
            aliases: vec![],
        });
    }

    // Pass 2: build measure_name → semantic_model_name map and add model edges
    let mut measure_to_sem: HashMap<String, String> = HashMap::new();
    for (sm, yaml_path) in semantic_models {
        let sem_id = format!("semantic_model.{}", sm.name);
        let Some(&sem_idx) = gb.node_map.get(&sem_id) else {
            continue;
        };
        for measure in &sm.measures {
            if let Some(existing) = measure_to_sem.get(&measure.name) {
                if existing != &sm.name {
                    crate::warn!(
                        "measure '{}' defined in both semantic_model '{}' and '{}'; \
                         linking metrics to '{}'",
                        measure.name,
                        existing,
                        sm.name,
                        existing
                    );
                }
            } else {
                measure_to_sem.insert(measure.name.clone(), sm.name.clone());
            }
        }
        // Add edge: model_node → semantic_model_node
        if let Some(model_ref) = &sm.model
            && let Some((model_name, version)) = parse_exposure_ref(model_ref)
        {
            let dep_idx = gb.get_or_create_phantom_ref(&model_name, version, yaml_path.as_path());
            gb.graph
                .add_edge(dep_idx, sem_idx, EdgeData::direct(EdgeType::Ref));
        }
    }

    // Pass 3: register metric nodes
    for (metric, yaml_path) in metrics {
        let unique_id = format!("metric.{}", metric.name);
        if gb.node_map.contains_key(&unique_id) {
            continue;
        }
        gb.add_node(NodeData {
            unique_id,
            label: metric.label.as_deref().unwrap_or(&metric.name).to_string(),
            node_type: NodeType::Metric,
            file_path: Some(yaml_path.clone()),
            description: metric.description.clone(),
            materialization: None,
            tags: vec![],
            columns: vec![],
            exposure: None,
            aliases: vec![],
        });
    }

    // Pass 4: add semantic_model → metric and metric → metric edges
    for (metric, yaml_path) in metrics {
        let metric_id = format!("metric.{}", metric.name);
        let Some(&metric_idx) = gb.node_map.get(&metric_id) else {
            continue;
        };
        // Link to semantic models via measure references (Simple, Conversion, …).
        // Deduplicate: a conversion metric's base_measure and conversion_measure may
        // both belong to the same semantic model, which would otherwise add the edge twice.
        // Use seen-set + ordered iteration to keep insertion order deterministic.
        let mut seen_sem_indices = std::collections::HashSet::new();
        for measure_name in metric.measure_refs() {
            let Some(sem_name) = measure_to_sem.get(measure_name) else {
                continue;
            };
            let sem_id = format!("semantic_model.{}", sem_name);
            let Some(&sem_idx) = gb.node_map.get(&sem_id) else {
                continue;
            };
            if seen_sem_indices.insert(sem_idx) {
                gb.graph
                    .add_edge(sem_idx, metric_idx, EdgeData::direct(EdgeType::Ref));
            }
        }
        // Ratio/Derived/Conversion/Cumulative: link to upstream metrics (deduplicated,
        // preserving original order so graph insertion is deterministic)
        let mut seen_metric_refs = std::collections::HashSet::new();
        for dep_metric_name in metric.metric_refs() {
            if !seen_metric_refs.insert(dep_metric_name) {
                continue;
            }
            let dep_id = format!("metric.{}", dep_metric_name);
            let dep_idx = if let Some(&idx) = gb.node_map.get(&dep_id) {
                idx
            } else {
                crate::warn!(
                    "unresolved metric ref '{}' from metric '{}'",
                    dep_metric_name,
                    metric.name
                );
                gb.add_node(NodeData {
                    unique_id: dep_id,
                    label: dep_metric_name.to_string(),
                    node_type: NodeType::Phantom,
                    file_path: Some(yaml_path.clone()),
                    description: None,
                    materialization: None,
                    tags: vec![],
                    columns: vec![],
                    exposure: None,
                    aliases: vec![],
                })
            };
            gb.graph
                .add_edge(dep_idx, metric_idx, EdgeData::direct(EdgeType::Ref));
        }
    }

    // Pass 5: register saved_query nodes and add metric → saved_query edges
    for (sq, yaml_path) in saved_queries {
        let sq_id = format!("saved_query.{}", sq.name);
        if gb.node_map.contains_key(&sq_id) {
            continue;
        }
        let sq_idx = gb.add_node(NodeData {
            unique_id: sq_id.clone(),
            label: sq.label.as_deref().unwrap_or(&sq.name).to_string(),
            node_type: NodeType::SavedQuery,
            file_path: Some(yaml_path.clone()),
            description: sq.description.clone(),
            materialization: None,
            tags: vec![],
            columns: vec![],
            exposure: None,
            aliases: vec![],
        });
        if let Some(qp) = &sq.query_params {
            for metric_name in &qp.metrics {
                let metric_dep_id = format!("metric.{}", metric_name);
                let dep_idx = if let Some(&idx) = gb.node_map.get(&metric_dep_id) {
                    idx
                } else {
                    crate::warn!(
                        "unresolved metric ref '{}' in saved_query '{}'",
                        metric_name,
                        sq.name
                    );
                    gb.add_node(NodeData {
                        unique_id: metric_dep_id,
                        label: metric_name.clone(),
                        node_type: NodeType::Phantom,
                        file_path: Some(yaml_path.clone()),
                        description: None,
                        materialization: None,
                        tags: vec![],
                        columns: vec![],
                        exposure: None,
                        aliases: vec![],
                    })
                };
                gb.graph
                    .add_edge(dep_idx, sq_idx, EdgeData::direct(EdgeType::Ref));
            }
        }
    }
}
