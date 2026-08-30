use super::*;

#[test]
fn finds_transitive_and_alias_reachability() {
    let sql = r#"
        {% macro inner() %}{{ ref('inner') }}{% endmacro %}
        {% macro outer() %}{{ inner() }}{% endmacro %}
        {% set alias = outer %}
        {{ alias() }}
    "#;
    let reachable = reachable_local_macros_with_prefix(sql, "", None, None).unwrap();
    assert_eq!(reachable.0, HashSet::from(["inner".into(), "outer".into()]));
}

#[test]
fn excludes_unused_definitions() {
    let sql = r#"
        {% macro used() %}{{ ref('used') }}{% endmacro %}
        {% macro unused() %}{{ ref('unused') }}{% endmacro %}
        {{ used() }}
    "#;
    let reachable = reachable_local_macros_with_prefix(sql, "", None, None).unwrap();
    assert_eq!(reachable.0, HashSet::from(["used".into()]));
}

#[test]
fn returns_empty_for_model_without_local_macros() {
    let (reachable, local_macro_count) =
        reachable_local_macros_with_prefix("SELECT 1", "", None, None).unwrap();
    assert!(reachable.is_empty());
    assert_eq!(local_macro_count, 0);
}

#[test]
fn reuses_known_model_roots_without_compiling_model_source() {
    let sql = r#"
        {% macro inner() %}{% endmacro %}
        {% macro outer() %}{{ inner() }}{% endmacro %}
        {{ outer() }}
    "#;
    let roots = HashSet::from(["outer".to_owned()]);
    let (reachable, _local_macro_count, root_compile_performed, _definition_compile_count) =
        reachable_local_macros_with_status_and_prefix(sql, "", None, Some(&roots)).unwrap();
    assert_eq!(reachable, HashSet::from(["inner".into(), "outer".into()]));
    assert!(!root_compile_performed);
}

#[test]
fn reuses_known_prefix_roots_without_compiling_model_source() {
    let prefix = r#"
        {% macro choose() %}{{ left() }}{% endmacro %}
    "#;
    let sql = r#"
        {% macro left() %}{{ ref('left') }}{% endmacro %}
        {% macro right() %}{{ ref('right') }}{% endmacro %}
        {{ choose() }}
    "#;
    let roots = HashSet::from(["choose".to_owned()]);
    let (reachable, _count, root_compile_performed, _definition_compile_count) =
        reachable_local_macros_with_status_and_prefix(sql, prefix, None, Some(&roots)).unwrap();
    assert_eq!(reachable, HashSet::from(["left".into()]));
    assert!(!root_compile_performed);
}

#[test]
fn follows_higher_order_collection_and_conditional_symbols() {
    let sql = r#"
        {% macro first() %}{% endmacro %}
        {% macro second() %}{% endmacro %}
        {% macro invoke(callback) %}{{ callback() }}{% endmacro %}
        {% set callbacks = [first, second] %}
        {% set selected = first if execute else second %}
        {{ invoke(callbacks[0]) }}{{ selected() }}
    "#;
    let reachable = reachable_local_macros_with_prefix(sql, "", None, None).unwrap();
    assert_eq!(
        reachable.0,
        HashSet::from(["first".into(), "second".into(), "invoke".into()])
    );
}

#[test]
fn delegates_shadowing_to_minijinja_free_symbol_analysis() {
    let sql = r#"
        {% macro shadowed() %}{{ ref('shadowed') }}{% endmacro %}
        {% macro caller() %}
            {% set shadowed = 'local value' %}{{ shadowed }}
        {% endmacro %}
        {{ caller() }}
    "#;
    let reachable = reachable_local_macros_with_prefix(sql, "", None, None).unwrap();
    assert_eq!(reachable.0, HashSet::from(["caller".into()]));
}

#[test]
fn reports_uncompilable_source_as_unknown() {
    let sql = "{% macro broken() %}{{ ref('unfinished') }}";
    assert!(reachable_local_macros_with_prefix(sql, "", None, None).is_none());
}

#[test]
fn reports_reachable_definition_compile_failure_as_unknown() {
    let sql = "{% bad other %}{{ broken() }}{% bad_other %}";
    let spans = [
        ModelMacroSpan {
            start: 0,
            end: 15,
            opening_end: 0,
            closing_start: 15,
            name: "broken".to_owned(),
        },
        ModelMacroSpan {
            start: 29,
            end: sql.len(),
            opening_end: 29,
            closing_start: sql.len(),
            name: "other".to_owned(),
        },
    ];
    assert!(reachable_local_macros_with_prefix(sql, "", Some(&spans), None).is_none());
}

#[test]
fn skips_dense_inert_definitions_without_local_symbol_candidates() {
    let env = Environment::new();
    let local_names: HashSet<String> = (0..128)
        .map(|index| format!("runtime_macro_{index:03}"))
        .collect();
    let source = "{% macro runtime_macro_127() %}unused{% endmacro %}";
    let (dependencies, compiled) =
        local_macro_dependencies_with_status(&env, source, "runtime_macro_127", &local_names)
            .unwrap();
    assert!(dependencies.is_empty());
    assert!(!compiled);
}

#[test]
fn unused_uncompilable_prefix_definition_does_not_poison_reachable_analysis() {
    let source = "{% macro used() %}used{% endmacro %}\n".to_owned()
        + "{% macro unused() %}{% invalid_statement %}{% endmacro %}";
    let prepared = PreparedMacroPrefix::new(&source);
    let roots = HashSet::from(["used".to_owned()]);
    let plan =
        macro_reachability_with_prepared_prefix("{{ used() }}", &prepared, None, Some(&roots))
            .unwrap();
    assert_eq!(plan.prefix_scopes, HashSet::from(["used".to_owned()]));
    assert_eq!(prepared.initialized_definition_count(), 1);
}

#[test]
fn compiles_when_default_argument_references_another_macro() {
    let env = Environment::new();
    let local_names = HashSet::from(["caller".to_owned(), "default_value".to_owned()]);
    let source = "{% macro caller(value=default_value()) %}{{ value }}{% endmacro %}";
    let (dependencies, compiled) =
        local_macro_dependencies_with_status(&env, source, "caller", &local_names).unwrap();
    assert_eq!(dependencies, HashSet::from(["default_value".to_owned()]));
    assert!(compiled);
}

#[test]
fn substring_collision_remains_conservative() {
    let env = Environment::new();
    let local_names = HashSet::from(["a".to_owned(), "caller".to_owned()]);
    let source = "{% macro caller() %}a label{% endmacro %}";
    let (_, compiled) =
        local_macro_dependencies_with_status(&env, source, "caller", &local_names).unwrap();
    assert!(compiled);
}

#[test]
fn skips_all_definition_expansion_when_every_macro_is_a_root() {
    let mut sql = String::new();
    let mut roots = HashSet::new();
    for index in 0..128 {
        let name = format!("runtime_macro_{index:03}");
        roots.insert(name.clone());
        sql.push_str(&format!(
            "{{% macro {name}() %}}{{{{ ref('model_{index:03}') }}}}{{% endmacro %}}\n"
        ));
    }

    let (reachable, local_macro_count, root_compiled, definition_compiles) =
        reachable_local_macros_with_status_and_prefix(&sql, "", None, Some(&roots)).unwrap();
    assert_eq!(reachable, roots);
    assert_eq!(local_macro_count, 128);
    assert!(!root_compiled);
    assert_eq!(definition_compiles, 0);
}

#[test]
fn keeps_partial_reachability_scoped_against_all_local_macros() {
    let mut sql = String::new();
    for index in 0..128 {
        sql.push_str(&format!(
            "{{% macro runtime_macro_{index:03}() %}}unused{{% endmacro %}}\n"
        ));
    }
    sql.push_str("{{ runtime_macro_000() }}{{ runtime_macro_001() }}{{ runtime_macro_002() }}");

    let (reachable, local_macro_count) =
        reachable_local_macros_with_prefix(&sql, "", None, None).unwrap();
    assert_eq!(local_macro_count, 128);
    assert_eq!(reachable.len(), 3);
    assert!(
        reachable
            .iter()
            .all(|name| name.starts_with("runtime_macro_00"))
    );
}
