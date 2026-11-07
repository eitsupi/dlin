//! Compare two public-API routes to the same FROM-clause column.
use std::collections::{BTreeMap, BTreeSet};

use dlin_sqllineage::{Candidate, Ident, Lineage, OutputName, Severity, Shape, Unresolved};
use serde_json::{Value, json};

use super::{direct_using_inputs, identifiers, scopes_joins, stars, support::Case, target_columns};

// Identical observations share one record, but every probe key is explicit.
// These are exact observations, not conditions for accepting a mismatch.
#[derive(serde::Deserialize)]
struct RecordedDifference {
    cases: Vec<String>,
    observation: Value,
}

fn normalize(lineage: &mut Lineage, wrapper: &str) {
    if let Lineage::Partial { unresolved, .. } = lineage {
        for item in unresolved {
            if let Unresolved::Ambiguous { candidates, .. } = item {
                for candidate in candidates.iter_mut() {
                    if matches!(candidate, Candidate::Derived(Some(name)) if name.value == wrapper)
                    {
                        *candidate = Candidate::Derived(None);
                    }
                }
            }
        }
    }
}

fn normalize_duplicate_boundary(
    lineage: &mut Lineage,
    boundary: &Candidate,
    probe: &Ident,
    dialect: dlin_sqllineage::Dialect,
) {
    // Only a whole-column ambiguity for this lookup can denote the duplicate
    // input output. Preserve ambiguities inside expressions for other columns,
    // as well as mixed known/unresolved lineage and other uncertainty markers.
    if let Lineage::Partial { known, unresolved } = lineage
        && known.is_empty()
        && let [Unresolved::Ambiguous { column, candidates }] = unresolved.as_mut_slice()
        && column.column_key(dialect) == probe.column_key(dialect)
    {
        *candidates = vec![boundary.clone()];
    }
}

fn observation(
    case: &Case,
    sql: &str,
    catalog: bool,
    wrapper: &str,
    boundary: Option<&Candidate>,
    probe: &Ident,
) -> Value {
    match case.analyze_sql(sql, catalog) {
        Err(error) => json!({"error": error}),
        Ok(statement) => {
            let errors: Vec<_> = statement
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.severity == Severity::Error)
                .collect();
            let lineages: Vec<_> = statement
                .output
                .columns
                .into_iter()
                .map(|column| {
                    let mut lineage = column.lineage;
                    normalize(&mut lineage, wrapper);
                    if let Some(boundary) = boundary {
                        normalize_duplicate_boundary(&mut lineage, boundary, probe, case.dialect());
                    }
                    lineage
                })
                .collect();
            json!({"lineages": lineages, "errors": errors})
        }
    }
}

fn invalid(observation: &Value) -> bool {
    observation.get("error").is_some()
        || observation["errors"]
            .as_array()
            .is_none_or(|errors| !errors.is_empty())
        || observation["lineages"]
            .as_array()
            .is_none_or(|lineages| lineages.len() != 1)
}

#[test]
fn direct_and_star_from_clause_lookups_agree() {
    let cases: Vec<_> = identifiers::cases()
        .into_iter()
        .chain(scopes_joins::cases())
        .chain(stars::cases())
        .chain(target_columns::cases())
        .collect();
    let mut failures = BTreeMap::<String, Value>::new();
    let mut skipped = BTreeMap::<String, Vec<String>>::new();
    let mut probes = 0;
    let mut inputs_count = 0;
    for case in &cases {
        let (inputs, reasons) = direct_using_inputs::inputs(case);
        if !reasons.is_empty() {
            skipped.insert(case.name.clone(), reasons);
        }
        inputs_count += inputs.len();
        for input in inputs {
            for catalog in [false, true] {
                let mode = if catalog { "catalog" } else { "no_catalog" };
                let prefix = format!("{}/{}/{mode}", case.name, input.label);
                let star_sql = format!("{} SELECT * FROM {}", input.with_sql, input.from_sql);
                let mut names = input.names.clone();
                let mut listed_keys = BTreeMap::<String, usize>::new();
                match case.analyze_sql(&star_sql, catalog) {
                    Ok(statement) => {
                        let open = matches!(statement.output.shape, Shape::Open { .. });
                        names.extend(statement.output.columns.into_iter().filter_map(|column| {
                            match column.name {
                                OutputName::Explicit(name) | OutputName::Inferred(name) => {
                                    *listed_keys
                                        .entry(name.column_key(case.dialect()).into_owned())
                                        .or_default() += 1;
                                    Some(name)
                                }
                                OutputName::Anonymous => None,
                            }
                        }));
                        // With an entirely unknown star, compare the routes for
                        // a hypothetical column. This does not assert that the
                        // column exists, only that wrapping preserves lookup.
                        if names.is_empty() && open {
                            names.push(Ident::unquoted("lineage_probe_column"));
                        }
                    }
                    Err(error) => {
                        skipped
                            .entry(prefix.clone())
                            .or_default()
                            .push(format!("star discovery failed: {error}"));
                    }
                }
                let mut seen = BTreeSet::new();
                names.retain(|name| seen.insert(name.column_key(case.dialect()).into_owned()));
                if names.is_empty() {
                    skipped
                        .entry(prefix)
                        .or_default()
                        .push("no input column name to probe".into());
                    continue;
                }
                let mut wrapper = "lineage_probe_wrapper".to_owned();
                while case.sql().contains(&wrapper) {
                    wrapper.push('_');
                }
                for name in names {
                    let name_sql = super::star_invariants::reference(&name);
                    let direct_sql = format!(
                        "{} SELECT {name_sql} FROM {}",
                        input.with_sql, input.from_sql
                    );
                    let through_sql = format!(
                        "{} SELECT {name_sql} FROM (SELECT * FROM {}) {wrapper}",
                        input.with_sql, input.from_sql
                    );
                    let boundary = input.boundary.as_ref().filter(|_| {
                        listed_keys
                            .get(name.column_key(case.dialect()).as_ref())
                            .is_some_and(|count| *count > 1)
                    });
                    let direct = observation(case, &direct_sql, catalog, &wrapper, boundary, &name);
                    let through =
                        observation(case, &through_sql, catalog, &wrapper, boundary, &name);
                    probes += 1;
                    if direct != through || invalid(&direct) || invalid(&through) {
                        let kind = if invalid(&direct) || invalid(&through) {
                            "oracle"
                        } else {
                            "lineage"
                        };
                        assert!(failures.insert(format!("{prefix}/{name_sql}"), json!({
                            "kind": kind, "direct_sql": direct_sql, "star_sql": through_sql,
                            "direct": direct, "star": through,
                        })).is_none());
                    }
                }
            }
        }
    }
    let groups: Vec<RecordedDifference> =
        serde_json::from_str(include_str!("direct_using_invariant_failures.json")).unwrap();
    let mut recorded = BTreeMap::new();
    for group in groups {
        assert!(!group.cases.is_empty(), "recorded difference has no probes");
        for key in group.cases {
            assert!(
                recorded.insert(key, group.observation.clone()).is_none(),
                "duplicate recorded probe"
            );
        }
    }
    let recorded_skips: BTreeMap<String, Vec<String>> =
        serde_json::from_str(include_str!("direct_using_invariant_skips.json")).unwrap();
    for (key, failure) in &recorded {
        assert!(
            failures.contains_key(key),
            "{key}: comparison now agrees; remove recorded failure {failure}"
        );
    }
    eprintln!(
        "{} direct/star fixtures, {inputs_count} FROM inputs, {probes} probes, {} recorded differences; {} applicability records",
        cases.len(),
        failures.len(),
        skipped.len()
    );
    eprintln!(
        "DIRECT_USING_SKIPS={}",
        serde_json::to_string(&skipped).unwrap()
    );
    if failures != recorded {
        eprintln!(
            "DIRECT_USING_OBSERVED={}",
            serde_json::to_string(&failures).unwrap()
        );
    }
    assert_eq!(
        skipped, recorded_skips,
        "direct/star applicability changed; review skipped inputs explicitly"
    );
    assert_eq!(
        failures, recorded,
        "direct/star observations changed; review differences without weakening the comparison"
    );
}

#[test]
fn normalization_only_removes_the_synthetic_wrapper_identity() {
    let original = Candidate::Derived(Some(Ident::unquoted("original")));
    let mut lineage = Lineage::Partial {
        known: vec![],
        unresolved: vec![Unresolved::Ambiguous {
            column: Ident::unquoted("id"),
            candidates: vec![
                original.clone(),
                Candidate::Cte(Ident::unquoted("wrapper")),
                Candidate::Derived(Some(Ident::unquoted("wrapper"))),
            ],
        }],
    };
    normalize(&mut lineage, "wrapper");
    assert!(
        matches!(lineage, Lineage::Partial { unresolved, .. } if matches!(&unresolved[..], [Unresolved::Ambiguous { candidates, .. }] if candidates == &[original, Candidate::Cte(Ident::unquoted("wrapper")), Candidate::Derived(None)]))
    );
}

#[test]
fn duplicate_boundary_normalization_preserves_inner_expression_ambiguity() {
    let boundary = Candidate::Derived(Some(Ident::unquoted("input")));
    let mut lineage = Lineage::Partial {
        known: vec![],
        unresolved: vec![Unresolved::Ambiguous {
            column: Ident::unquoted("inner_column"),
            candidates: vec![
                Candidate::Cte(Ident::unquoted("a")),
                Candidate::Cte(Ident::unquoted("b")),
            ],
        }],
    };
    let original = lineage.clone();
    normalize_duplicate_boundary(
        &mut lineage,
        &boundary,
        &Ident::unquoted("output_name"),
        dlin_sqllineage::Dialect::Generic,
    );
    assert_eq!(lineage, original);
    normalize_duplicate_boundary(
        &mut lineage,
        &boundary,
        &Ident::unquoted("inner_column"),
        dlin_sqllineage::Dialect::Generic,
    );
    assert!(
        matches!(lineage, Lineage::Partial { unresolved, .. } if matches!(&unresolved[..], [Unresolved::Ambiguous { candidates, .. }] if candidates == &[boundary]))
    );
}
