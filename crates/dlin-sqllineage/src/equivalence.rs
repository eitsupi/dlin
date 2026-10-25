use std::collections::BTreeSet;

use crate::legacy_analyze as analyze;
use crate::types::{
    AnalyzeOptions, AnalyzeResult, ColumnMapping, ColumnOrigin, Dialect, TableRef, TransformKind,
};
use crate::{
    Analyzer, Candidate, DiagnosticKind, Ident, Lineage, OutputName, Relation, Shape,
    StatementAnalysis, StatementKind, Transform, Unresolved,
};

const SOURCES: &[(&str, &str)] = &[
    (
        "aggregate_clauses.rs",
        include_str!("../tests/aggregate_clauses.rs"),
    ),
    ("catalog.rs", include_str!("../tests/catalog.rs")),
    (
        "column_lineage.rs",
        include_str!("../tests/column_lineage.rs"),
    ),
    ("cte.rs", include_str!("../tests/cte.rs")),
    ("determinism.rs", include_str!("../tests/determinism.rs")),
    ("dialect.rs", include_str!("../tests/dialect.rs")),
    (
        "expr_coverage.rs",
        include_str!("../tests/expr_coverage.rs"),
    ),
    ("join_merge.rs", include_str!("../tests/join_merge.rs")),
    ("lateral.rs", include_str!("../tests/lateral.rs")),
    ("merge.rs", include_str!("../tests/merge.rs")),
    (
        "merged_join_sources.rs",
        include_str!("../tests/merged_join_sources.rs"),
    ),
    (
        "multi_hop_transform.rs",
        include_str!("../tests/multi_hop_transform.rs"),
    ),
    (
        "multi_statement.rs",
        include_str!("../tests/multi_statement.rs"),
    ),
    (
        "qualifier_resolution.rs",
        include_str!("../tests/qualifier_resolution.rs"),
    ),
    ("regressions.rs", include_str!("../tests/regressions.rs")),
    (
        "setexpr_coverage.rs",
        include_str!("../tests/setexpr_coverage.rs"),
    ),
    (
        "sqlparser_063.rs",
        include_str!("../tests/sqlparser_063.rs"),
    ),
    ("star_order.rs", include_str!("../tests/star_order.rs")),
    (
        "table_lineage.rs",
        include_str!("../tests/table_lineage.rs"),
    ),
    ("update.rs", include_str!("../tests/update.rs")),
    (
        "window_functions.rs",
        include_str!("../tests/window_functions.rs"),
    ),
];

const DIALECTS: [Dialect; 4] = [
    Dialect::Generic,
    Dialect::PostgreSql,
    Dialect::BigQuery,
    Dialect::Snowflake,
];

#[test]
fn existing_sql_corpus_has_legacy_public_correspondence() {
    let corpus = sql_literals_from_existing_tests();
    assert!(
        corpus.len() >= 200,
        "SQL corpus unexpectedly shrank: found {} distinct SQL literals",
        corpus.len()
    );

    let mut accepted_pairs = 0usize;
    let mut semantic_error_pairs = 0usize;
    let mut parse_rejections = 0usize;

    for dialect in DIALECTS {
        for sql in &corpus {
            let legacy = analyze(
                sql,
                AnalyzeOptions {
                    dialect,
                    ..AnalyzeOptions::default()
                },
            );
            let current = Analyzer::new(dialect).analyze(sql);

            match (legacy, current) {
                (Ok(old_results), Ok(new_analysis)) => {
                    assert_eq!(
                        old_results.len(),
                        new_analysis.statements.len(),
                        "statement count differs for {dialect:?}: {sql}"
                    );
                    for (old, new) in old_results.iter().zip(&new_analysis.statements) {
                        assert_statement_equivalent(old, new, dialect, sql);
                    }
                    accepted_pairs += 1;
                }
                (Err(old_error), Ok(new_analysis)) => {
                    assert!(
                        !is_parser_error(&old_error.message),
                        "legacy parser rejected SQL while public accepted it for {dialect:?}: {}\nSQL: {sql}",
                        old_error.message
                    );
                    let errors: Vec<_> = new_analysis
                        .statements
                        .iter()
                        .flat_map(|statement| &statement.diagnostics)
                        .filter(|diagnostic| diagnostic.severity == crate::Severity::Error)
                        .collect();
                    assert!(
                        !errors.is_empty(),
                        "legacy semantic error has no public error diagnostic for {dialect:?}: {}\nSQL: {sql}",
                        old_error.message
                    );
                    for statement in &new_analysis.statements {
                        if statement
                            .diagnostics
                            .iter()
                            .any(|diagnostic| diagnostic.severity == crate::Severity::Error)
                        {
                            assert!(
                                statement.output.columns.is_empty(),
                                "semantic-error statement exposes output columns for {dialect:?}: {sql}"
                            );
                        }
                    }
                    assert_semantic_error_kind(&old_error.message, &errors, dialect, sql);
                    accepted_pairs += 1;
                    semantic_error_pairs += 1;
                }
                (Err(old_error), Err(_new_error)) => {
                    assert!(
                        is_parser_error(&old_error.message),
                        "legacy semantic error was not represented as a public statement diagnostic for {dialect:?}: {}\nSQL: {sql}",
                        old_error.message
                    );
                    parse_rejections += 1;
                }
                (Ok(_), Err(new_error)) => panic!(
                    "public parser rejected SQL accepted by legacy API for {dialect:?}: {}\nSQL: {sql}",
                    new_error.message
                ),
            }
        }
    }

    assert!(
        accepted_pairs >= 200,
        "only {accepted_pairs} dialect/SQL pairs were compared successfully"
    );
    println!(
        "public correspondence corpus: {} SQL literals, {} dialect/SQL comparisons, {} semantic-error comparisons, {} parse rejections",
        corpus.len(),
        accepted_pairs,
        semantic_error_pairs,
        parse_rejections
    );
}

#[test]
fn root_shape_does_not_copy_statement_wide_legacy_star_flag() {
    let sql = "SELECT t.id FROM t JOIN (SELECT * FROM u) q ON t.id = q.id";
    let legacy = analyze(sql, AnalyzeOptions::default()).expect("legacy SQL parses");
    let current = Analyzer::new(Dialect::Generic)
        .analyze(sql)
        .expect("public SQL parses");

    assert!(legacy[0].columns.has_unresolved_stars);
    assert!(current.statements[0].has_unresolved_stars);
    assert!(matches!(&current.statements[0].output.shape, Shape::Closed));
}

fn assert_statement_equivalent(
    old: &AnalyzeResult,
    new: &StatementAnalysis,
    dialect: Dialect,
    sql: &str,
) {
    assert_eq!(
        new.kind,
        expected_statement_kind(&old.statement_type),
        "statement kind differs for {dialect:?}: {sql}"
    );
    assert_eq!(
        legacy_relation_key_option(old.tables.output.as_ref()),
        relation_key_option(new.written.as_ref()),
        "written relation differs for {dialect:?}: {sql}"
    );
    assert_eq!(
        legacy_relation_keys(&old.tables.inputs),
        relation_keys(&new.read),
        "read relations differ for {dialect:?}: {sql}"
    );

    let old_columns: Vec<_> = old
        .columns
        .mappings
        .iter()
        .filter(|mapping| !is_actual_wildcard_sentinel(mapping))
        .collect();
    assert_eq!(
        old_columns.len(),
        new.output.columns.len(),
        "output column count differs for {dialect:?}: {sql}"
    );

    for (index, (old_column, new_column)) in old_columns.iter().zip(&new.output.columns).enumerate()
    {
        assert_output_name(
            &old_column.target.column,
            &new_column.name,
            dialect,
            index,
            sql,
        );
        assert_eq!(
            new_column.transform,
            expected_transform(&old_column.transform),
            "transform differs at output {index} for {dialect:?}: {sql}"
        );
        assert_lineage_equivalent(
            &old_column.sources,
            &new_column.lineage,
            index,
            dialect,
            sql,
        );
    }
    assert_unexpanded_stars_equivalent(old, new, dialect, sql);
}

/// One expected or actual unexpanded star: relation, position, known sources
/// and unresolved origins.
type StarKey = (Option<String>, Option<usize>, Vec<String>, Vec<String>);

/// Legacy star mappings correspond to the unexpanded stars of the output
/// shape: one entry per relation behind the star (or one without a relation),
/// positioned after the last known output column and unknown once any star
/// has been seen.
fn assert_unexpanded_stars_equivalent(
    old: &AnalyzeResult,
    new: &StatementAnalysis,
    dialect: Dialect,
    sql: &str,
) {
    let mut expected: Vec<StarKey> = Vec::new();
    let mut columns_before = 0usize;
    let mut seen_star = false;
    for mapping in &old.columns.mappings {
        if !is_actual_wildcard_sentinel(mapping) {
            columns_before += 1;
            continue;
        }
        let after_ordinal = if seen_star || columns_before == 0 {
            None
        } else {
            Some(columns_before - 1)
        };
        seen_star = true;
        let mut relations = Vec::new();
        legacy_star_relations(&mapping.sources, &mut relations);
        if relations.is_empty() {
            relations.push(None);
        }
        let (known, mut unresolved) = expected_lineage(&mapping.sources);
        // The legacy marker for "the star expands to unknown columns" is an
        // unbound column named `*`; the public result names it explicitly.
        for item in &mut unresolved {
            if item == "unbound:*" {
                *item = "unsupported:star expansion".to_owned();
            }
        }
        unresolved.sort();
        for relation in relations {
            expected.push((
                legacy_relation_key_option(relation),
                after_ordinal,
                known.clone(),
                unresolved.clone(),
            ));
        }
    }
    let actual: Vec<StarKey> = match &new.output.shape {
        Shape::Open { unexpanded } => unexpanded
            .iter()
            .map(|star| {
                let (known, unresolved) = actual_lineage(&star.lineage);
                (
                    relation_key_option(star.relation.as_ref()),
                    star.after_ordinal,
                    known,
                    unresolved,
                )
            })
            .collect(),
        Shape::Closed => Vec::new(),
    };
    assert_eq!(
        expected, actual,
        "unexpanded stars differ for {dialect:?}: {sql}"
    );
}

fn legacy_star_relations<'a>(
    origins: &'a [ColumnOrigin],
    relations: &mut Vec<Option<&'a TableRef>>,
) {
    for origin in origins {
        match origin {
            ColumnOrigin::Wildcard { table } => relations.push(Some(table)),
            ColumnOrigin::Recursive { base_sources } => {
                legacy_star_relations(base_sources, relations);
            }
            _ => {}
        }
    }
}

fn assert_output_name(old: &str, new: &OutputName, dialect: Dialect, index: usize, sql: &str) {
    if old == "?column?" {
        assert_eq!(
            new,
            &OutputName::Anonymous,
            "anonymous output mismatch at {index} for {dialect:?}: {sql}"
        );
        return;
    }
    let actual = match new {
        OutputName::Explicit(ident) | OutputName::Inferred(ident) => ident,
        OutputName::Anonymous => panic!(
            "public reports anonymous output where legacy named it {old:?} at {index} for {dialect:?}: {sql}"
        ),
    };
    let matches = if actual.quote.is_some() {
        actual.value == old
    } else {
        Ident::unquoted(old).column_key(dialect) == actual.column_key(dialect)
    };
    assert!(
        matches,
        "output name differs at {index}: old={old:?}, public={actual:?}, {dialect:?}: {sql}"
    );
}

fn assert_lineage_equivalent(
    old: &[ColumnOrigin],
    new: &Lineage,
    index: usize,
    dialect: Dialect,
    sql: &str,
) {
    let (expected_known, expected_unresolved) = expected_lineage(old);
    let (actual_known, actual_unresolved) = actual_lineage(new);
    assert_eq!(
        expected_known, actual_known,
        "known sources differ at output {index} for {dialect:?}: {sql}"
    );
    assert_eq!(
        expected_unresolved, actual_unresolved,
        "unresolved origins differ at output {index} for {dialect:?}: {sql}"
    );
}

fn expected_lineage(origins: &[ColumnOrigin]) -> (Vec<String>, Vec<String>) {
    fn visit(
        origin: &ColumnOrigin,
        known: &mut BTreeSet<String>,
        unresolved: &mut BTreeSet<String>,
    ) {
        match origin {
            ColumnOrigin::Concrete { table, column } => {
                known.insert(format!(
                    "{}:{}",
                    legacy_table_key(table),
                    column.to_lowercase()
                ));
            }
            ColumnOrigin::Ambiguous { column, candidates } => {
                if candidates.is_empty() {
                    unresolved.insert(format!("unbound:{}", column.to_lowercase()));
                } else {
                    let mut relations = legacy_relation_keys(candidates);
                    relations.sort();
                    unresolved.insert(format!(
                        "ambiguous:{}:{}",
                        column.to_lowercase(),
                        relations.join("|")
                    ));
                }
            }
            ColumnOrigin::Wildcard { table } => {
                unresolved.insert(format!("star:{}", legacy_table_key(table)));
            }
            ColumnOrigin::NamedWildcard { table, column } => {
                unresolved.insert(format!(
                    "named-star:{}:{}",
                    legacy_table_key(table),
                    column.to_lowercase()
                ));
            }
            ColumnOrigin::SourceFree { .. } => {}
            ColumnOrigin::Recursive { base_sources } => {
                unresolved.insert("recursive".to_owned());
                for source in base_sources {
                    visit(source, known, unresolved);
                }
            }
        }
    }

    let mut known = BTreeSet::new();
    let mut unresolved = BTreeSet::new();
    for origin in origins {
        visit(origin, &mut known, &mut unresolved);
    }
    (
        known.into_iter().collect(),
        unresolved.into_iter().collect(),
    )
}

fn actual_lineage(lineage: &Lineage) -> (Vec<String>, Vec<String>) {
    let (known, unresolved) = match lineage {
        Lineage::Complete(known) => (known.as_slice(), &[][..]),
        Lineage::Partial { known, unresolved } => (known.as_slice(), unresolved.as_slice()),
    };
    let known = known
        .iter()
        .map(|source| {
            format!(
                "{}:{}",
                relation_key(&source.relation),
                source.column.value.to_lowercase()
            )
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let unresolved = unresolved
        .iter()
        .map(unresolved_key)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    (known, unresolved)
}

fn unresolved_key(origin: &Unresolved) -> String {
    match origin {
        Unresolved::BehindStar { relation } => format!("star:{}", relation_key(relation)),
        Unresolved::NamedBehindStar { relation, column } => {
            format!(
                "named-star:{}:{}",
                relation_key(relation),
                column.value.to_lowercase()
            )
        }
        Unresolved::Ambiguous { column, candidates } => {
            let mut relations = candidates
                .iter()
                .map(|candidate| match candidate {
                    Candidate::Relation(relation) => relation_key(relation),
                    Candidate::Cte(name) => format!("::{}", name.value.to_lowercase()),
                    Candidate::Derived(name) => format!(
                        "::{}",
                        name.as_ref()
                            .map_or_else(String::new, |ident| ident.value.to_lowercase())
                    ),
                })
                .collect::<Vec<_>>();
            relations.sort();
            format!(
                "ambiguous:{}:{}",
                column.value.to_lowercase(),
                relations.join("|")
            )
        }
        Unresolved::Unbound { column } => format!("unbound:{}", column.value.to_lowercase()),
        Unresolved::Recursive => "recursive".to_owned(),
        Unresolved::RowValue { alias } => format!("unbound:{}", alias.value.to_lowercase()),
        Unresolved::Unsupported { what } => format!("unsupported:{what}"),
    }
}

fn is_actual_wildcard_sentinel(mapping: &ColumnMapping) -> bool {
    mapping.target.column == "*" && mapping.sources.iter().any(origin_has_star)
}

fn origin_has_star(origin: &ColumnOrigin) -> bool {
    match origin {
        ColumnOrigin::Wildcard { .. } => true,
        ColumnOrigin::Ambiguous { column, .. } => column == "*",
        ColumnOrigin::Recursive { base_sources } => base_sources.iter().any(origin_has_star),
        ColumnOrigin::Concrete { .. }
        | ColumnOrigin::NamedWildcard { .. }
        | ColumnOrigin::SourceFree { .. } => false,
    }
}

fn expected_statement_kind(kind: &crate::types::StatementType) -> StatementKind {
    match kind {
        crate::types::StatementType::Query => StatementKind::Query,
        crate::types::StatementType::Insert => StatementKind::Insert,
        crate::types::StatementType::CreateTable => StatementKind::CreateTableAs,
        crate::types::StatementType::Update => StatementKind::Update,
        crate::types::StatementType::Delete => StatementKind::Delete,
        crate::types::StatementType::Merge => StatementKind::Merge,
        crate::types::StatementType::Other => StatementKind::Other,
    }
}

fn expected_transform(transform: &TransformKind) -> Transform {
    match transform {
        TransformKind::Direct => Transform::Direct,
        TransformKind::Expression => Transform::Expression,
        TransformKind::Aggregation => Transform::Aggregation,
        TransformKind::Conditional => Transform::Conditional,
        TransformKind::Window => Transform::Window,
        TransformKind::Unknown => Transform::Unknown,
    }
}

fn relation_keys(relations: &[Relation]) -> Vec<String> {
    let mut keys = relations.iter().map(relation_key).collect::<Vec<_>>();
    keys.sort();
    keys.dedup();
    keys
}

fn legacy_relation_key_option(relation: Option<&TableRef>) -> Option<String> {
    relation.map(legacy_table_key)
}

fn relation_key_option(relation: Option<&Relation>) -> Option<String> {
    relation.map(relation_key)
}

fn legacy_relation_keys(relations: &[TableRef]) -> Vec<String> {
    let mut keys = relations.iter().map(legacy_table_key).collect::<Vec<_>>();
    keys.sort();
    keys.dedup();
    keys
}

fn relation_key(relation: &Relation) -> String {
    format!(
        "{}:{}:{}",
        relation
            .catalog
            .as_ref()
            .map_or_else(String::new, |ident| ident.value.to_lowercase()),
        relation
            .schema
            .as_ref()
            .map_or_else(String::new, |ident| ident.value.to_lowercase()),
        relation.name.value.to_lowercase()
    )
}

fn legacy_table_key(table: &TableRef) -> String {
    format!(
        "{}:{}:{}",
        table.catalog.as_deref().unwrap_or_default().to_lowercase(),
        table.schema.as_deref().unwrap_or_default().to_lowercase(),
        table.table.to_lowercase()
    )
}

fn assert_semantic_error_kind(
    message: &str,
    errors: &[&crate::Diagnostic],
    dialect: Dialect,
    sql: &str,
) {
    if message.starts_with("set operation arity mismatch:") {
        assert!(
            errors.iter().any(|diagnostic| matches!(
                &diagnostic.kind,
                DiagnosticKind::SetOperationArityMismatch { .. }
            )),
            "legacy arity error did not become typed public arity diagnostic for {dialect:?}: {sql}"
        );
    } else {
        assert!(
            errors.iter().any(|diagnostic| matches!(
                &diagnostic.kind,
                DiagnosticKind::UnsupportedSyntax { .. }
                    | DiagnosticKind::SetOperationArityMismatch { .. }
            )),
            "legacy semantic error has an unrelated public diagnostic for {dialect:?}: {sql}"
        );
    }
}

fn is_parser_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("parser error:") || message.contains("tokenizer error:")
}

fn sql_literals_from_existing_tests() -> BTreeSet<String> {
    SOURCES
        .iter()
        .flat_map(|(_, source)| rust_string_literals(source))
        .map(|literal| literal.trim().to_owned())
        .filter(|literal| looks_like_sql(literal))
        .collect()
}

fn looks_like_sql(literal: &str) -> bool {
    let upper = literal.trim_start().to_ascii_uppercase();
    [
        "SELECT ", "SELECT\n", "WITH ", "WITH\n", "INSERT ", "INSERT\n", "CREATE ", "CREATE\n",
        "UPDATE ", "UPDATE\n", "DELETE ", "DELETE\n", "MERGE ", "MERGE\n", "FROM ", "FROM\n",
    ]
    .iter()
    .any(|prefix| upper.starts_with(prefix))
}

/// Collect ordinary and raw Rust string literals while skipping comments.
fn rust_string_literals(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut literals = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"//") {
            index = source[index..]
                .find('\n')
                .map_or(bytes.len(), |offset| index + offset + 1);
        } else if bytes[index..].starts_with(b"/*") {
            index = skip_block_comment(bytes, index);
        } else if let Some((value, end)) = raw_string_at(source, index) {
            literals.push(value);
            index = end;
        } else if bytes[index] == b'"' {
            let (value, end) = normal_string_at(source, index);
            literals.push(value);
            index = end;
        } else if bytes[index] == b'\'' {
            index = skip_char_literal(source, index).unwrap_or(index + 1);
        } else {
            index += source[index..].chars().next().map_or(1, char::len_utf8);
        }
    }
    literals
}

fn skip_block_comment(bytes: &[u8], mut index: usize) -> usize {
    let mut depth = 1usize;
    index += 2;
    while index < bytes.len() && depth > 0 {
        if bytes[index..].starts_with(b"/*") {
            depth += 1;
            index += 2;
        } else if bytes[index..].starts_with(b"*/") {
            depth -= 1;
            index += 2;
        } else {
            index += 1;
        }
    }
    index
}

fn raw_string_at(source: &str, start: usize) -> Option<(String, usize)> {
    let bytes = source.as_bytes();
    if bytes[start] != b'r' {
        return None;
    }
    let mut delimiter = start + 1;
    while delimiter < bytes.len() && bytes[delimiter] == b'#' {
        delimiter += 1;
    }
    if bytes.get(delimiter) != Some(&b'"') {
        return None;
    }
    let hashes = delimiter - start - 1;
    let closing = format!("\"{}", "#".repeat(hashes));
    let body_start = delimiter + 1;
    let relative_end = source[body_start..].find(&closing)?;
    let body_end = body_start + relative_end;
    Some((
        source[body_start..body_end].to_owned(),
        body_end + closing.len(),
    ))
}

fn normal_string_at(source: &str, start: usize) -> (String, usize) {
    let bytes = source.as_bytes();
    let mut output = String::new();
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => return (output, index + 1),
            b'\\' => {
                index += 1;
                if index >= bytes.len() {
                    break;
                }
                match bytes[index] {
                    b'n' => output.push('\n'),
                    b'r' => output.push('\r'),
                    b't' => output.push('\t'),
                    b'0' => output.push('\0'),
                    b'\\' => output.push('\\'),
                    b'"' => output.push('"'),
                    b'\'' => output.push('\''),
                    b'\n' => {
                        index += 1;
                        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                            index += 1;
                        }
                        continue;
                    }
                    b'x' if index + 2 < bytes.len() => {
                        if let Ok(value) = u8::from_str_radix(&source[index + 1..index + 3], 16) {
                            output.push(char::from(value));
                            index += 2;
                        }
                    }
                    b'u' if bytes.get(index + 1) == Some(&b'{') => {
                        let digits_start = index + 2;
                        if let Some(close) = source[digits_start..].find('}') {
                            if let Ok(value) = u32::from_str_radix(
                                source[digits_start..digits_start + close]
                                    .replace('_', "")
                                    .as_str(),
                                16,
                            ) && let Some(character) = char::from_u32(value)
                            {
                                output.push(character);
                            }
                            index = digits_start + close;
                        }
                    }
                    other => output.push(char::from(other)),
                }
                index += 1;
            }
            _ => {
                let character = source[index..].chars().next().expect("valid source UTF-8");
                output.push(character);
                index += character.len_utf8();
            }
        }
    }
    (output, index)
}

fn skip_char_literal(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut index = start + 1;
    if index >= bytes.len() || bytes[index] == b'\n' {
        return None;
    }
    if bytes[index] == b'\\' {
        index += 1;
        if index >= bytes.len() {
            return None;
        }
        if bytes[index] == b'u' && bytes.get(index + 1) == Some(&b'{') {
            index += 2;
            while index < bytes.len() && bytes[index] != b'}' {
                index += 1;
            }
            index += usize::from(index < bytes.len());
        } else {
            index += 1;
        }
    } else {
        index += source[index..].chars().next()?.len_utf8();
    }
    (bytes.get(index) == Some(&b'\'')).then_some(index + 1)
}
