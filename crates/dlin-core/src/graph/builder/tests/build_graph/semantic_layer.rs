use super::*;
use std::fs;

#[test]
fn test_build_graph_semantic_layer_full() {
    let (_tmp, project_dir) = setup_temp_project();

    // Add a semantic layer YAML alongside the existing schema.yml
    let models_dir = project_dir.join("models");
    fs::write(
        models_dir.join("semantic.yml"),
        r#"
semantic_models:
  - name: orders
    description: Order semantic model
    model: ref('orders')
    measures:
      - name: order_count
      - name: order_total

metrics:
  - name: orders
    type: simple
    type_params:
      measure: order_count
  - name: order_total
    type: simple
    type_params:
      measure: order_total
  - name: revenue_per_order
    type: ratio
    type_params:
      numerator: order_total
      denominator: orders

saved_queries:
  - name: order_kpis
    description: Key order KPIs
    query_params:
      metrics:
        - orders
        - order_total
        - revenue_per_order
"#,
    )
    .unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![
            project_dir.join("models/stg_orders.sql"),
            project_dir.join("models/orders.sql"),
        ],
        yaml_files: vec![
            project_dir.join("models/schema.yml"),
            project_dir.join("models/semantic.yml"),
        ],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();

    // Node type counts
    let counts: HashMap<NodeType, usize> =
        graph.node_indices().fold(HashMap::new(), |mut acc, i| {
            *acc.entry(graph[i].node_type).or_insert(0) += 1;
            acc
        });

    // 1 source + 2 models + 1 semantic_model + 3 metrics + 1 saved_query = 8
    assert_eq!(*counts.get(&NodeType::Source).unwrap_or(&0), 1, "sources");
    assert_eq!(*counts.get(&NodeType::Model).unwrap_or(&0), 2, "models");
    assert_eq!(
        *counts.get(&NodeType::SemanticModel).unwrap_or(&0),
        1,
        "semantic_models"
    );
    assert_eq!(*counts.get(&NodeType::Metric).unwrap_or(&0), 3, "metrics");
    assert_eq!(
        *counts.get(&NodeType::SavedQuery).unwrap_or(&0),
        1,
        "saved_queries"
    );

    // Verify semantic_model.orders exists and is linked to model.orders
    let sem_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "semantic_model.orders")
        .expect("semantic_model.orders not found");
    let model_orders_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "model.orders")
        .expect("model.orders not found");
    assert!(
        graph.contains_edge(model_orders_idx, sem_idx),
        "model.orders → semantic_model.orders edge missing"
    );

    // Verify metric.orders is linked to semantic_model.orders
    let metric_orders_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "metric.orders")
        .expect("metric.orders not found");
    assert!(
        graph.contains_edge(sem_idx, metric_orders_idx),
        "semantic_model.orders → metric.orders edge missing"
    );

    // Verify revenue_per_order depends on order_total and orders metrics
    let ratio_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "metric.revenue_per_order")
        .expect("metric.revenue_per_order not found");
    let metric_total_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "metric.order_total")
        .expect("metric.order_total not found");
    assert!(
        graph.contains_edge(metric_total_idx, ratio_idx),
        "metric.order_total → metric.revenue_per_order edge missing"
    );
    assert!(
        graph.contains_edge(metric_orders_idx, ratio_idx),
        "metric.orders → metric.revenue_per_order edge missing"
    );

    // Verify saved_query.order_kpis is linked to all 3 metrics
    let sq_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "saved_query.order_kpis")
        .expect("saved_query.order_kpis not found");
    assert!(
        graph.contains_edge(metric_orders_idx, sq_idx),
        "metric.orders → saved_query.order_kpis edge missing"
    );
    assert!(
        graph.contains_edge(metric_total_idx, sq_idx),
        "metric.order_total → saved_query.order_kpis edge missing"
    );
    assert!(
        graph.contains_edge(ratio_idx, sq_idx),
        "metric.revenue_per_order → saved_query.order_kpis edge missing"
    );
}

#[test]
fn test_build_graph_semantic_layer_metric_reference_shapes() {
    let (_tmp, project_dir) = setup_temp_project();

    let models_dir = project_dir.join("models");
    fs::write(
        models_dir.join("semantic_refs.yml"),
        r#"
semantic_models:
  - name: orders
    model: ref('orders')
    measures:
      - name: order_total
      - name: order_count
      - name: customer_count

metrics:
  - name: order_total
    type: simple
    type_params:
      measure:
        name: order_total
        fill_nulls_with: 0
  - name: orders
    type: simple
    type_params:
      measure: order_count
  - name: customers
    type: simple
    type_params:
      measure: customer_count
  - name: derived_kpi
    type: derived
    type_params:
      metrics:
        - name: order_total
        - orders
        - customers
"#,
    )
    .unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![
            project_dir.join("models/stg_orders.sql"),
            project_dir.join("models/orders.sql"),
        ],
        yaml_files: vec![
            project_dir.join("models/schema.yml"),
            project_dir.join("models/semantic_refs.yml"),
        ],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();

    let sem_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "semantic_model.orders")
        .expect("semantic_model.orders not found");
    let metric_total_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "metric.order_total")
        .expect("metric.order_total not found");
    assert!(
        graph.contains_edge(sem_idx, metric_total_idx),
        "semantic_model.orders → metric.order_total edge missing"
    );

    let derived_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "metric.derived_kpi")
        .expect("metric.derived_kpi not found");
    for metric_id in ["metric.order_total", "metric.orders", "metric.customers"] {
        let metric_idx = graph
            .node_indices()
            .find(|&i| graph[i].unique_id == metric_id)
            .unwrap_or_else(|| panic!("{metric_id} not found"));
        assert!(
            graph.contains_edge(metric_idx, derived_idx),
            "{metric_id} → metric.derived_kpi edge missing"
        );
    }
}
