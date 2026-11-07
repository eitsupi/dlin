//! Metamorphic checks use only the public analyzer and fixture inputs.
//! Recorded failures contain actual observations, never correct SQL expectations.
use std::collections::{BTreeMap, BTreeSet};

use dlin_sqllineage::{
    DiagnosticKind, Ident, Lineage, OutputName, Quote, Shape, SourceColumn, Unresolved,
};
use serde_json::{Value, json};

use super::{identifiers, scopes_joins, stars, support::Case, target_columns};

pub(super) fn reference(name: &Ident) -> String {
    match name.quote {
        None => name.value.clone(),
        Some(Quote::Double) => format!("\"{}\"", name.value.replace('"', "\"\"")),
        Some(Quote::Backtick) => format!("`{}`", name.value.replace('`', "``")),
        Some(Quote::Bracket) => format!("[{}]", name.value.replace(']', "]]")),
        Some(Quote::Single) => format!("'{}'", name.value.replace('\'', "''")),
    }
}

fn named(name: &OutputName) -> Option<&Ident> {
    match name {
        OutputName::Explicit(name) | OutputName::Inferred(name) => Some(name),
        OutputName::Anonymous => None,
    }
}

fn known(lineage: &Lineage) -> &[SourceColumn] {
    match lineage {
        Lineage::Complete(sources) | Lineage::Partial { known: sources, .. } => sources,
    }
}

// Include named candidates behind unknown stars: they must not reappear after
// a denied EXCLUDE/ILIKE even when neither side has catalog metadata.
fn contributions(lineage: &Lineage, name: &Ident) -> Vec<SourceColumn> {
    let mut sources = known(lineage).to_vec();
    if let Lineage::Partial { unresolved, .. } = lineage {
        sources.extend(unresolved.iter().filter_map(|unresolved| match unresolved {
            Unresolved::NamedBehindStar { relation, column } => Some(SourceColumn {
                relation: relation.clone(),
                column: column.clone(),
            }),
            Unresolved::BehindStar { relation } => Some(SourceColumn {
                relation: relation.clone(),
                column: name.clone(),
            }),
            _ => None,
        }));
        // A relation that may own the name is a possible contribution too.
        sources.extend(unresolved.iter().flat_map(|unresolved| {
            match unresolved {
                Unresolved::Ambiguous { candidates, .. } => candidates
                    .iter()
                    .filter_map(|candidate| match candidate {
                        dlin_sqllineage::Candidate::Relation(relation) => Some(SourceColumn {
                            relation: relation.clone(),
                            column: name.clone(),
                        }),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            }
        }));
    }
    sources
}

// The fixture SQL has no comments. Only remove star filter syntax, leaving
// nested projections, JOINs, replacement expressions and aliases untouched.
fn without_filters(sql: &str) -> String {
    let mut result = sql.to_owned();
    for keyword in [" EXCLUDE (", " ILIKE '"] {
        while let Some(start) = result.find(keyword) {
            let body = start + keyword.len();
            let end = if keyword.contains("EXCLUDE") {
                result[body..].find(')').map(|offset| body + offset + 1)
            } else {
                result[body..].find('\'').map(|offset| body + offset + 1)
            }
            .expect("fixture star filter is terminated");
            result.replace_range(start..end, "");
        }
    }
    result
}

fn query(sql: &str) -> &str {
    // A DML statement cannot be used as a derived query. Check its source q;
    // target-list renaming remains covered by the original handwritten case.
    if sql.starts_with("INSERT ") || sql.starts_with("CREATE ") {
        &sql[sql
            .find("SELECT ")
            .expect("star fixture has a SELECT source")..]
    } else {
        sql
    }
}

/// Whether a LIKE pattern (`%` any run, `_` one character) matches `value`,
/// ignoring case.
fn like(pattern: &[char], value: &[char]) -> bool {
    match pattern.split_first() {
        None => value.is_empty(),
        Some(('%', rest)) => {
            like(rest, value)
                || value
                    .split_first()
                    .is_some_and(|(_, tail)| like(pattern, tail))
        }
        Some((part, rest)) => value.split_first().is_some_and(|(first, tail)| {
            (*part == '_' || part.eq_ignore_ascii_case(first)) && like(rest, tail)
        }),
    }
}

/// Whether the columns a star cannot enumerate may still carry `name`, judged
/// from the fixture's SQL text alone. `Some(true)`: nothing written restricts
/// the name, so an unknown part of the output may publish it next to the
/// column the star lists. `Some(false)`: an ILIKE pattern that does not match
/// the name keeps the unknown columns from publishing it. `None`: an EXCLUDE,
/// REPLACE, RENAME or USING item mentions the name, or the statement is a set
/// operation (which takes its names from its left operand and appends the
/// right one's beyond it). What the unknown columns may carry then depends on
/// which relation the item applies to, which the text does not tell, and either
/// answer is sound; the hand-written cases pin those shapes exactly.
fn unknown_may_carry(sql: &str, name: &Ident, dialect: dlin_sqllineage::Dialect) -> Option<bool> {
    let key = name.column_key(dialect).into_owned();
    let mentions = |word: &str| {
        word.rsplit('.')
            .next()
            .is_some_and(|last| last.trim_matches(['"', '`']).eq_ignore_ascii_case(&key))
    };
    for keyword in [" EXCLUDE (", " REPLACE (", " RENAME (", " USING ("] {
        let mut rest = sql;
        while let Some(start) = rest.find(keyword) {
            let body_start = start + keyword.len();
            let mut depth = 1;
            let mut end = rest.len();
            for (offset, character) in rest[body_start..].char_indices() {
                match character {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                if depth == 0 {
                    end = body_start + offset;
                    break;
                }
            }
            if rest[body_start..end]
                .split(|c: char| c == ',' || c.is_whitespace() || c == '(' || c == ')')
                .any(mentions)
            {
                return None;
            }
            rest = &rest[end..];
        }
    }
    if [" UNION ", " INTERSECT ", " EXCEPT "]
        .iter()
        .any(|keyword| sql.contains(keyword))
    {
        return None;
    }
    if let Some(start) = sql.find(" ILIKE '") {
        let body = &sql[start + " ILIKE '".len()..];
        let pattern: Vec<char> = body[..body.find('\'').unwrap_or(0)].chars().collect();
        let value: Vec<char> = name.value.chars().collect();
        if !like(&pattern, &value) {
            return Some(false);
        }
    }
    Some(true)
}

/// A lookup that finds the name ambiguous, whatever the candidates.
fn ambiguous_lookup(lineages: &[Lineage], name: &Ident, dialect: dlin_sqllineage::Dialect) -> bool {
    matches!(lineages, [Lineage::Partial { known, unresolved }]
        if known.is_empty()
            && matches!(&unresolved[..], [Unresolved::Ambiguous { column, .. }]
                if column.column_key(dialect) == name.column_key(dialect)))
}

struct Checks {
    failures: BTreeMap<String, Value>,
    count: usize,
    candidate_oracle_unavailable: usize,
    invalid_filter_controls: usize,
    counts: BTreeMap<&'static str, usize>,
}

impl Checks {
    fn check(&mut self, key: String, expected: Value, actual: Value) {
        self.count += 1;
        let suffix = key.rsplit('/').next().unwrap();
        let kind = if suffix.starts_with("width_") {
            "width"
        } else if suffix.starts_with("name_") {
            "lookup"
        } else if suffix.starts_with("excluded_") {
            "excluded"
        } else if suffix == "control_errors" {
            "control"
        } else {
            "baseline"
        };
        *self.counts.entry(kind).or_default() += 1;
        if actual != expected {
            assert!(
                self.failures
                    .insert(key, json!({"expected": expected, "actual": actual}))
                    .is_none()
            );
        }
    }
}

fn check_case(case: &Case, with_catalog: bool, checks: &mut Checks) {
    let sql = query(case.sql());
    let label = format!(
        "{}/{}",
        case.name,
        if with_catalog {
            "catalog"
        } else {
            "no_catalog"
        }
    );
    let result = case.analyze_sql(sql, with_catalog).expect(&label);
    let errors = result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == dlin_sqllineage::Severity::Error)
        .collect::<Vec<_>>();
    checks.check(format!("{label}/baseline_errors"), json!([]), json!(errors));
    if !errors.is_empty() {
        return;
    }
    let width = result.output.columns.len();
    let probe_width = if matches!(result.output.shape, Shape::Open { .. }) {
        width.max(1)
    } else {
        width
    };
    for count in [probe_width, probe_width + 1] {
        let literals = (0..count).map(|_| "1").collect::<Vec<_>>().join(", ");
        let tail = if count == 0 {
            " FROM (SELECT 1) empty_projection"
        } else {
            ""
        };
        let union = format!("SELECT * FROM ({sql}) d UNION ALL SELECT {literals}{tail}");
        let actual = case.analyze_sql(&union, with_catalog);
        let mismatch = actual.and_then(|statement| {
            if statement.diagnostics.iter().any(|diagnostic| {
                diagnostic.severity == dlin_sqllineage::Severity::Error
                    && !matches!(
                        diagnostic.kind,
                        DiagnosticKind::SetOperationArityMismatch { .. }
                    )
            }) {
                return Err(format!(
                    "unexpected semantic diagnostics: {:?}",
                    statement.diagnostics
                ));
            }
            Ok(statement
                .diagnostics
                .into_iter()
                .filter_map(|d| {
                    if let DiagnosticKind::SetOperationArityMismatch { left, right } = d.kind {
                        Some((left, right))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>())
        });
        let expected = if matches!(result.output.shape, Shape::Closed) && count != width {
            vec![(width, count)]
        } else {
            vec![]
        };
        checks.check(
            format!("{label}/width_{count}"),
            json!(Ok::<_, String>(expected)),
            json!(mismatch),
        );
    }
    for column in &result.output.columns {
        let Some(name) = named(&column.name) else {
            continue;
        };
        if result
            .output
            .columns
            .iter()
            .filter(|column| {
                named(&column.name).is_some_and(|other| {
                    other.column_key(case.dialect()) == name.column_key(case.dialect())
                })
            })
            .count()
            != 1
        {
            continue;
        }
        let probe = format!("SELECT {} FROM ({sql}) d", reference(name));
        let actual = case.analyze_sql(&probe, with_catalog).map(|statement| {
            statement
                .output
                .columns
                .into_iter()
                .map(|column| column.lineage)
                .collect::<Vec<_>>()
        });
        // A name the star lists is that column, unless the columns the star
        // cannot enumerate may carry it too: then two providers publish it and
        // it denotes none of them. Which name an unknown part can carry
        // follows from the modifiers written in the fixture, not from the
        // analysis.
        let open = matches!(result.output.shape, Shape::Open { .. });
        let may_carry = if open {
            unknown_may_carry(sql, name, case.dialect())
        } else {
            Some(false)
        };
        let found_ambiguous = actual
            .as_ref()
            .is_ok_and(|lineages| ambiguous_lookup(lineages, name, case.dialect()));
        let schema_dependent = matches!(&column.lineage, Lineage::Partial { unresolved, .. }
            if unresolved.iter().any(|item| matches!(item, Unresolved::SchemaDependent { .. })));
        let (expected, actual) = match may_carry {
            // P4: a possible NATURAL key is one schema-dependent output slot,
            // not a competing independent column. A wrapper keeps that value
            // and its unresolved schema contribution exactly.
            _ if schema_dependent => (
                json!(Ok::<_, String>(vec![column.lineage.clone()])),
                json!(actual),
            ),
            Some(true) => (
                json!("ambiguous"),
                if found_ambiguous {
                    json!("ambiguous")
                } else {
                    json!(actual)
                },
            ),
            Some(false) => (
                json!(Ok::<_, String>(vec![column.lineage.clone()])),
                json!(actual),
            ),
            // Only some relations lose the name: either answer is sound.
            None if found_ambiguous => (json!("ambiguous"), json!("ambiguous")),
            None => (
                json!(Ok::<_, String>(vec![column.lineage.clone()])),
                json!(actual),
            ),
        };
        checks.check(
            format!("{label}/name_{}", reference(name)),
            expected,
            actual,
        );
    }
    let control_sql = without_filters(sql);
    if control_sql == sql {
        return;
    }
    let control = case.analyze_sql(&control_sql, with_catalog).expect(&label);
    let control_errors = control
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == dlin_sqllineage::Severity::Error)
        .collect::<Vec<_>>();
    // Removing EXCLUDE from these otherwise valid UNION fixtures deliberately
    // restores an extra left column. Pin that control error, rather than
    // silently accepting arbitrary failed controls or calling it an engine bug.
    let control_widths = if with_catalog {
        match case.name.as_str() {
            "aliased_merged_exclude_matches_union_width" | "left_merged_exclude_right_alias" => {
                Some((3, 2))
            }
            "qualified_relation_star_keeps_implicit_range_variable"
            | "qualified_star_exclude_uses_column_key"
            | "unmerged_exclude_matches_union_width" => Some((2, 1)),
            _ => None,
        }
    } else {
        None
    };
    let expected_control_errors = control_widths
        .map(|(left, right)| dlin_sqllineage::Diagnostic {
            severity: dlin_sqllineage::Severity::Error,
            kind: DiagnosticKind::SetOperationArityMismatch { left, right },
            location: None,
        })
        .into_iter()
        .collect::<Vec<_>>();
    checks.check(
        format!("{label}/control_errors"),
        json!(expected_control_errors),
        json!(control_errors),
    );
    if !control_errors.is_empty() {
        if control_widths.is_some() {
            checks.invalid_filter_controls += 1;
            // Every pinned fixture removes id from the UNION's left output.
            // Probe the valid filtered query directly: no id contribution exists.
            let probe = format!("SELECT id FROM ({sql}) d");
            let actual = case
                .analyze_sql(&probe, with_catalog)
                .and_then(|statement| {
                    if statement
                        .diagnostics
                        .iter()
                        .any(|diagnostic| diagnostic.severity == dlin_sqllineage::Severity::Error)
                    {
                        return Err(format!(
                            "unexpected semantic diagnostics: {:?}",
                            statement.diagnostics
                        ));
                    }
                    Ok(statement
                        .output
                        .columns
                        .iter()
                        .flat_map(|column| contributions(&column.lineage, &Ident::unquoted("id")))
                        .collect::<Vec<_>>())
                });
            checks.check(
                format!("{label}/excluded_id"),
                json!(Ok::<_, String>(Vec::<SourceColumn>::new())),
                json!(actual),
            );
        }
        return;
    }
    let mut seen = BTreeSet::new();
    for column in &control.output.columns {
        let Some(name) = named(&column.name) else {
            continue;
        };
        let key = name.column_key(case.dialect()).into_owned();
        if !seen.insert(key) {
            continue;
        }
        // Correct handwritten expectations identify contributions which survive
        // this filter (e.g. a self-join's other range variable). Everything else
        // exposed by the unfiltered control must not leak through a reference.
        let allowed = case
            .expected_named_lineages(name)
            .iter()
            .flat_map(|lineage| contributions(lineage, name))
            .collect::<Vec<_>>();
        // Removing a fixture catalog can make a former non-owner a possible
        // provider (notably in USING chains). Its candidates are no longer
        // governed by the catalog-backed handwritten expectation.
        let control_sources = if with_catalog || !case.has_catalog() {
            contributions(&column.lineage, name)
        } else {
            if contributions(&column.lineage, name).len() > known(&column.lineage).len() {
                checks.candidate_oracle_unavailable += 1;
            }
            known(&column.lineage).to_vec()
        };
        let forbidden = control_sources
            .into_iter()
            .filter(|source| !allowed.contains(source))
            .collect::<Vec<_>>();
        if forbidden.is_empty() {
            continue;
        }
        let probe = format!("SELECT {} FROM ({sql}) d", reference(name));
        let actual = case
            .analyze_sql(&probe, with_catalog)
            .and_then(|statement| {
                if statement
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.severity == dlin_sqllineage::Severity::Error)
                {
                    return Err(format!(
                        "unexpected semantic diagnostics: {:?}",
                        statement.diagnostics
                    ));
                }
                Ok(statement
                    .output
                    .columns
                    .iter()
                    .flat_map(|column| contributions(&column.lineage, name))
                    .filter(|source| forbidden.contains(source))
                    .collect::<Vec<_>>())
            });
        checks.check(
            format!("{label}/excluded_{}", reference(name)),
            json!(Ok::<_, String>(Vec::<SourceColumn>::new())),
            json!(actual),
        );
    }
}

#[test]
fn star_width_lookup_and_filter_invariants() {
    let cases = identifiers::cases()
        .into_iter()
        .chain(scopes_joins::cases())
        .chain(stars::cases())
        .chain(target_columns::cases());
    let mut checks = Checks {
        failures: BTreeMap::new(),
        count: 0,
        candidate_oracle_unavailable: 0,
        invalid_filter_controls: 0,
        counts: BTreeMap::new(),
    };
    let mut count = 0;
    for case in cases.filter(|case| case.sql().contains('*')) {
        count += 1;
        for with_catalog in [false, true] {
            check_case(&case, with_catalog, &mut checks);
        }
    }
    let recorded: BTreeMap<String, Value> =
        serde_json::from_str(include_str!("star_invariant_failures.json"))
            .expect("recorded invariant failures are valid JSON");
    for (key, failure) in &recorded {
        assert!(
            checks.failures.contains_key(key),
            "{key}: invariant now holds; remove known failure {failure}"
        );
    }
    eprintln!(
        "{count} star fixtures, {} checks ({:?}), {} known failures; {} catalog-removed candidate checks lack an oracle; {} filter controls have pinned width errors",
        checks.count,
        checks.counts,
        checks.failures.len(),
        checks.candidate_oracle_unavailable,
        checks.invalid_filter_controls
    );
    assert_eq!(
        checks.failures,
        recorded,
        "star invariant failures changed; record new defects without changing the invariant:\n{}",
        serde_json::to_string_pretty(&checks.failures).unwrap()
    );
}
