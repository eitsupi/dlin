//! Expectations are written from SQL semantics, independently of engine output.
//!
//! Each case keeps its SQL, dialect, catalog and expected result together.
//! Source identifiers use the catalog's spelling when the catalog resolves them;
//! output names retain the projection's spelling. Predicate-only references do
//! not contribute value lineage. Add a one-line known_failure reason for an
//! engine defect, retaining the correct expectation; remove it when the runner
//! reports that the expectation now matches. Do not copy engine output here.

#[path = "expected_lineage/identifiers.rs"]
mod identifiers;
#[path = "expected_lineage/scopes_joins.rs"]
mod scopes_joins;
#[path = "expected_lineage/stars.rs"]
mod stars;
#[path = "expected_lineage/support.rs"]
mod support;

#[test]
fn hand_written_lineage_expectations() {
    let cases = identifiers::cases()
        .into_iter()
        .chain(scopes_joins::cases())
        .chain(stars::cases());
    let mut names = std::collections::BTreeSet::new();
    let mut failures = Vec::new();
    let mut known_failures = 0;
    for case in cases {
        if !names.insert(case.name.clone()) {
            failures.push(format!("duplicate case: {}", case.name));
        }
        if let Some(reason) = case.known_failure_reason() {
            known_failures += 1;
            eprintln!("known failure {}: {reason}", case.name);
        }
        if let Err(error) = case.run() {
            failures.push(format!("{}: {error}", case.name));
        }
    }
    eprintln!("{} cases, {known_failures} known failures", names.len());
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
