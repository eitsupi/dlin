mod common;

use common::{analyze_one, table};

#[test]
fn lateral_subquery_references_preceding_table() {
    let sql = "\
        SELECT t1.a, lat.b \
        FROM t1, LATERAL (SELECT b FROM t2 WHERE t2.id = t1.id) AS lat";
    let result = analyze_one(sql);
    let mut inputs = result.tables.inputs;
    inputs.sort();
    assert_eq!(inputs, vec![table("t1"), table("t2")]);
}

#[test]
fn lateral_join_table_inputs() {
    let sql = "\
        SELECT t1.x, l.y \
        FROM t1 \
        CROSS JOIN LATERAL (SELECT y FROM t2 WHERE t2.fk = t1.pk) AS l";
    let result = analyze_one(sql);
    let mut inputs = result.tables.inputs;
    inputs.sort();
    assert_eq!(inputs, vec![table("t1"), table("t2")]);
}
