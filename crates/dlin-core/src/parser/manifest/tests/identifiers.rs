#[test]
fn test_simplify_unique_id_model() {
    assert_eq!(
        simplify_unique_id("model.my_project.stg_orders", "model"),
        "model.stg_orders"
    );
}
#[test]
fn test_simplify_unique_id_source() {
    assert_eq!(
        simplify_unique_id("source.my_project.raw.orders", "source"),
        "source.raw.orders"
    );
}

#[test]
fn test_simplify_unique_id_short() {
    assert_eq!(
        simplify_unique_id("model.stg_orders", "model"),
        "model.stg_orders"
    );
}

#[test]
fn test_simplify_unique_id_source_short() {
    assert_eq!(
        simplify_unique_id("source.raw.orders", "source"),
        "source.raw.orders"
    );
}

#[test]
fn test_simplify_unique_id_test() {
    // test.project.test_name.hash -> test.test_name
    assert_eq!(
        simplify_unique_id(
            "test.jaffle_shop.not_null_orders_order_id.cf6c17daed",
            "test"
        ),
        "test.not_null_orders_order_id"
    );
}

#[test]
fn test_simplify_unique_id_test_short() {
    assert_eq!(
        simplify_unique_id("test.not_null_orders_order_id", "test"),
        "test.not_null_orders_order_id"
    );
}

#[test]
fn test_simplify_unique_id_versioned_model() {
    // dbt versioned model unique_ids: model.project.name.v{N} → model.name.v{N}
    assert_eq!(
        simplify_unique_id("model.my_project.my_model.v1", "model"),
        "model.my_model.v1"
    );
    assert_eq!(
        simplify_unique_id("model.my_project.my_model.v2", "model"),
        "model.my_model.v2"
    );
    // Unversioned model must still work
    assert_eq!(
        simplify_unique_id("model.my_project.stg_orders", "model"),
        "model.stg_orders"
    );
}

#[test]
fn test_infer_edge_type() {
    assert_eq!(
        infer_edge_type("source.my_project.raw.orders"),
        EdgeType::Source
    );
    assert_eq!(
        infer_edge_type("model.my_project.stg_orders"),
        EdgeType::Ref
    );
    assert_eq!(infer_edge_type("test.my_project.some_test"), EdgeType::Test);
    assert_eq!(infer_edge_type("seed.my_project.countries"), EdgeType::Ref);
}

#[test]
fn test_non_empty_string() {
    assert_eq!(non_empty_string(&None), None);
    assert_eq!(non_empty_string(&Some("".to_string())), None);
    assert_eq!(non_empty_string(&Some("  ".to_string())), None);
    assert_eq!(
        non_empty_string(&Some("hello".to_string())),
        Some("hello".to_string())
    );
}
