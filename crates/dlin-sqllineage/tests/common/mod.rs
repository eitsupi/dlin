use sqllineage::{AnalyzeOptions, AnalyzeResult, ColumnMapping, ColumnOrigin, TableRef, analyze};

#[allow(dead_code)]
pub fn analyze_one(sql: &str) -> AnalyzeResult {
    analyze(sql, AnalyzeOptions::default())
        .expect("SQL should parse")
        .into_iter()
        .next()
        .unwrap_or_default()
}

#[allow(dead_code)]
pub fn table(name: &str) -> TableRef {
    TableRef::new(name)
}

#[allow(dead_code)]
pub fn find_mapping<'a>(mappings: &'a [ColumnMapping], col: &str) -> &'a ColumnMapping {
    mappings
        .iter()
        .find(|m| m.target.column == col)
        .unwrap_or_else(|| panic!("no mapping for column '{col}'"))
}

#[allow(dead_code)]
pub fn concrete_sources(mapping: &ColumnMapping) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = mapping
        .sources
        .iter()
        .map(|s| match s {
            ColumnOrigin::Concrete { table, column } => (table.table.clone(), column.clone()),
            other => panic!("expected Concrete, got {other:?}"),
        })
        .collect();
    v.sort();
    v
}
