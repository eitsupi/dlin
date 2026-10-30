use std::borrow::Cow;

use dlin_sqllineage::{
    Analyzer, Catalog, CatalogColumn, CatalogEntry, Diagnostic, DiagnosticKind, Dialect, Ident,
    Lineage, OutputName, Quote, Relation, Severity, Shape, SourceColumn, StatementAnalysis,
    Unresolved,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpectedColumn {
    pub ordinal: Option<usize>,
    pub name: OutputName,
    pub lineage: Lineage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ExpectedOutput {
    columns: Vec<ExpectedColumn>,
    shape: Shape,
    diagnostics: Vec<Diagnostic>,
    has_unresolved_stars: Option<bool>,
}

pub struct Case {
    pub name: String,
    sql: String,
    dialect: Dialect,
    catalog: Option<FixtureCatalog>,
    expected: ExpectedOutput,
    known_failure: Option<KnownFailure>,
}

enum KnownFailure {
    Lineage {
        index: usize,
        reason: String,
    },
    MergedStar {
        sides: Vec<SourceColumn>,
        reason: String,
    },
}

impl KnownFailure {
    fn reason(&self) -> &str {
        match self {
            Self::Lineage { reason, .. } | Self::MergedStar { reason, .. } => reason,
        }
    }
}

impl Case {
    pub fn new(name: &str, sql: &str, dialect: Dialect, columns: Vec<ExpectedColumn>) -> Self {
        Self {
            name: name.into(),
            sql: sql.into(),
            dialect,
            catalog: None,
            expected: ExpectedOutput {
                columns,
                shape: Shape::Closed,
                diagnostics: vec![],
                has_unresolved_stars: None,
            },
            known_failure: None,
        }
    }

    pub fn catalog(mut self, entries: Vec<(Relation, Vec<Ident>)>) -> Self {
        self.catalog = Some(FixtureCatalog::new(&entries));
        self
    }

    pub fn catalog_alias(
        mut self,
        query: Relation,
        canonical: Relation,
        columns: Vec<Ident>,
    ) -> Self {
        self.catalog
            .get_or_insert_with(FixtureCatalog::default)
            .entries
            .push(FixtureEntry::new(query, canonical, columns));
        self
    }

    pub fn shape(mut self, shape: Shape) -> Self {
        self.expected.shape = shape;
        self
    }

    pub fn diagnostics(mut self, diagnostics: Vec<Diagnostic>) -> Self {
        self.expected.diagnostics = diagnostics;
        self
    }

    pub fn unresolved_stars(mut self, expected: bool) -> Self {
        self.expected.has_unresolved_stars = Some(expected);
        self
    }

    pub fn known_failure(self, reason: &str) -> Self {
        self.known_failure_at(0, reason)
    }

    pub fn known_failure_at(mut self, index: usize, reason: &str) -> Self {
        self.known_failure = Some(KnownFailure::Lineage {
            index,
            reason: reason.into(),
        });
        self
    }

    pub fn merged_star_failure(mut self, sides: Vec<SourceColumn>, reason: &str) -> Self {
        self.known_failure = Some(KnownFailure::MergedStar {
            sides,
            reason: reason.into(),
        });
        self
    }

    pub fn known_failure_reason(&self) -> Option<&str> {
        self.known_failure.as_ref().map(KnownFailure::reason)
    }

    pub fn run(&self) -> Result<(), String> {
        self.validate()?;
        let mut analyzer = Analyzer::new(self.dialect);
        if let Some(catalog) = &self.catalog {
            analyzer = analyzer.with_catalog(catalog);
        }
        let result = analyzer
            .analyze(&self.sql)
            .map_err(|error| format!("parse error (not an expectation mismatch): {error}"))?
            .into_single()
            .map_err(|error| format!("statement setup error: {error}"))?;
        self.compare(&result)
    }

    fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("setup error: a case needs a name".into());
        }
        if self.known_failure.as_ref().is_some_and(|failure| {
            failure.reason().trim().is_empty() || failure.reason().contains(['\n', '\r'])
        }) {
            return Err("setup error: known failure needs a nonempty one-line reason".into());
        }
        if let Some(catalog) = &self.catalog {
            for (index, entry) in catalog.entries.iter().enumerate() {
                if catalog.entries[..index]
                    .iter()
                    .any(|other| other.query == entry.query)
                {
                    return Err(format!(
                        "setup error: duplicate catalog relation {:?}",
                        entry.query
                    ));
                }
            }
        }
        match &self.known_failure {
            Some(KnownFailure::Lineage { index, .. }) if *index >= self.expected.columns.len() => {
                return Err("setup error: known failure column index is out of bounds".into());
            }
            Some(KnownFailure::MergedStar { sides, .. }) => {
                let Some(first) = self.expected.columns.first() else {
                    return Err("setup error: merged star needs an expected key".into());
                };
                if sides.len() < 2
                    || !matches!(first.name, OutputName::Explicit(_))
                    || self.expected.columns[1..]
                        .iter()
                        .any(|column| column.name == first.name)
                    || self.expected.shape != Shape::Closed
                    || !self.expected.diagnostics.is_empty()
                {
                    return Err("setup error: merged star expects one named key, closed shape, and no diagnostics".into());
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn compare(&self, actual: &StatementAnalysis) -> Result<(), String> {
        let actual_errors: Vec<_> = actual
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        let expected_errors: Vec<_> = self
            .expected
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        if !actual_errors.is_empty() && actual_errors != expected_errors {
            return Err(format!(
                "semantic error (not an expectation mismatch): {:?}",
                actual.diagnostics
            ));
        }
        let actual = ExpectedOutput {
            columns: actual
                .output
                .columns
                .iter()
                .map(|column| ExpectedColumn {
                    ordinal: column.ordinal,
                    name: column.name.clone(),
                    lineage: column.lineage.clone(),
                })
                .collect(),
            shape: actual.output.shape.clone(),
            diagnostics: actual.diagnostics.clone(),
            has_unresolved_stars: self
                .expected
                .has_unresolved_stars
                .map(|_| actual.has_unresolved_stars),
        };
        match &self.known_failure {
            None if actual == self.expected => Ok(()),
            None => Err(self.mismatch(&actual)),
            Some(KnownFailure::Lineage { index, reason }) => {
                let Some(column) = actual.columns.get(*index) else {
                    return Err(self.mismatch(&actual));
                };
                if column.lineage == self.expected.columns[*index].lineage {
                    return Err(Self::remove_flag(reason));
                }
                let mut checked = actual.clone();
                checked.columns[*index].lineage = self.expected.columns[*index].lineage.clone();
                if checked == self.expected {
                    Ok(())
                } else {
                    Err(self.mismatch(&actual))
                }
            }
            Some(KnownFailure::MergedStar { sides, reason }) => {
                self.compare_merged_star(&actual, sides, reason)
            }
        }
    }

    fn remove_flag(reason: &str) -> String {
        format!("known failure no longer reproduces; remove the known_failure flag: {reason}")
    }

    fn mismatch(&self, actual: &ExpectedOutput) -> String {
        format!(
            "expectation mismatch for SQL {:?}\nexpected: {:#?}\nactual: {actual:#?}",
            self.sql, self.expected
        )
    }

    fn compare_merged_star(
        &self,
        actual: &ExpectedOutput,
        sides: &[SourceColumn],
        reason: &str,
    ) -> Result<(), String> {
        let key = &self.expected.columns[0].name;
        let key_positions: Vec<_> = actual
            .columns
            .iter()
            .enumerate()
            .filter_map(|(index, column)| (&column.name == key).then_some(index))
            .collect();
        if key_positions.len() < 2 {
            return Err(Self::remove_flag(reason));
        }
        let key_lineages_match = key_positions.len() == sides.len()
            && key_positions.iter().zip(sides).all(|(index, side)| {
                actual.columns[*index].lineage == Lineage::Complete(vec![side.clone()])
            });
        let diagnostics_allowed = actual.diagnostics.iter().all(|diagnostic| {
            matches!(&diagnostic.kind, DiagnosticKind::DuplicateOutputName { name, ordinals }
                if diagnostic.severity == Severity::Warning
                    && key == &OutputName::Explicit(name.clone())
                    && ordinals.iter().all(|index| key_positions.contains(index)))
        });
        let non_keys: Vec<_> = actual
            .columns
            .iter()
            .filter(|column| &column.name != key)
            .map(|column| (&column.name, &column.lineage))
            .collect();
        let expected_non_keys: Vec<_> = self.expected.columns[1..]
            .iter()
            .map(|column| (&column.name, &column.lineage))
            .collect();
        if key_lineages_match
            && actual.shape == Shape::Closed
            && actual.has_unresolved_stars == self.expected.has_unresolved_stars
            && diagnostics_allowed
            && actual
                .columns
                .iter()
                .enumerate()
                .all(|(index, column)| column.ordinal == Some(index))
            && non_keys == expected_non_keys
        {
            Ok(())
        } else {
            Err(self.mismatch(actual))
        }
    }
}

#[derive(Default)]
struct FixtureCatalog {
    entries: Vec<FixtureEntry>,
}

struct FixtureEntry {
    query: Relation,
    canonical: Relation,
    columns: Vec<CatalogColumn>,
}

impl FixtureEntry {
    fn new(query: Relation, canonical: Relation, columns: Vec<Ident>) -> Self {
        Self {
            query,
            canonical,
            columns: columns
                .into_iter()
                .map(|name| CatalogColumn {
                    name,
                    data_type: None,
                    fields: None,
                })
                .collect(),
        }
    }
}

impl FixtureCatalog {
    fn new(entries: &[(Relation, Vec<Ident>)]) -> Self {
        Self {
            entries: entries
                .iter()
                .map(|(relation, columns)| {
                    FixtureEntry::new(relation.clone(), relation.clone(), columns.clone())
                })
                .collect(),
        }
    }
}

impl Catalog for FixtureCatalog {
    fn lookup(&self, relation: &Relation) -> Option<CatalogEntry<'_>> {
        self.entries
            .iter()
            .find(|entry| &entry.query == relation)
            .map(|entry| CatalogEntry {
                relation: entry.canonical.clone(),
                columns: Cow::Borrowed(&entry.columns),
            })
    }
}

pub fn u(value: &str) -> Ident {
    Ident::unquoted(value)
}

pub fn q(value: &str) -> Ident {
    Ident::quoted(value, Quote::Double)
}

/// Unquoted relation parts; quoted parts can be expressed with a Relation literal.
pub fn relation(value: &str) -> Relation {
    let parts: Vec<_> = value.split('.').collect();
    let (catalog, schema, name) = match parts.as_slice() {
        [name] => (None, None, *name),
        [schema, name] => (None, Some(u(schema)), *name),
        [catalog, schema, name] => (Some(u(catalog)), Some(u(schema)), *name),
        _ => panic!("expected one to three unquoted relation parts: {value}"),
    };
    assert!(
        parts.iter().all(|part| !part.is_empty()),
        "empty relation part: {value}"
    );
    Relation {
        catalog,
        schema,
        name: u(name),
    }
}

pub fn source(relation: Relation, column: Ident) -> SourceColumn {
    SourceColumn { relation, column }
}

pub fn complete(mut sources: Vec<SourceColumn>) -> Lineage {
    sources.sort();
    sources.dedup();
    Lineage::Complete(sources)
}

pub fn col(ordinal: usize, name: Ident, sources: Vec<SourceColumn>) -> ExpectedColumn {
    ExpectedColumn {
        ordinal: Some(ordinal),
        name: OutputName::Explicit(name),
        lineage: complete(sources),
    }
}

pub fn partial_col(
    ordinal: usize,
    name: Ident,
    mut known: Vec<SourceColumn>,
    unresolved: Vec<Unresolved>,
) -> ExpectedColumn {
    assert!(
        !unresolved.is_empty(),
        "partial lineage requires unresolved contributions"
    );
    known.sort();
    known.dedup();
    ExpectedColumn {
        ordinal: Some(ordinal),
        name: OutputName::Explicit(name),
        lineage: Lineage::Partial { known, unresolved },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dlin_sqllineage::{DiagnosticKind, Output, StatementKind};

    fn case() -> Case {
        Case::new(
            "runner example",
            "SELECT 1 AS value",
            Dialect::Generic,
            vec![col(0, u("value"), vec![])],
        )
    }

    fn statement() -> StatementAnalysis {
        StatementAnalysis {
            kind: StatementKind::Query,
            written: None,
            read: vec![],
            output: Output {
                columns: vec![dlin_sqllineage::OutputColumn {
                    ordinal: Some(0),
                    name: OutputName::Explicit(u("value")),
                    lineage: complete(vec![]),
                    transform: dlin_sqllineage::Transform::Expression,
                }],
                shape: Shape::Closed,
            },
            has_unresolved_stars: false,
            diagnostics: vec![],
        }
    }

    #[test]
    fn matching_case_passes_and_unflagged_mismatch_fails() {
        assert!(case().run().is_ok());
        let mut actual = statement();
        assert!(case().compare(&actual).is_ok());
        actual.output.columns[0].ordinal = Some(1);
        assert!(
            case()
                .compare(&actual)
                .unwrap_err()
                .contains("expectation mismatch")
        );
    }

    #[test]
    fn known_failure_requires_a_mismatch() {
        let case = case().known_failure("demonstration mismatch");
        assert!(
            case.compare(&statement())
                .unwrap_err()
                .contains("remove the known_failure flag")
        );
        let mut actual = statement();
        actual.output.columns[0].lineage = complete(vec![source(relation("t"), u("x"))]);
        assert!(case.compare(&actual).is_ok());
    }

    fn wrong_lineage() -> StatementAnalysis {
        let mut actual = statement();
        actual.output.columns[0].lineage = complete(vec![source(relation("t"), u("wrong"))]);
        actual
    }

    #[test]
    fn lineage_failure_rejects_unrelated_changes() {
        let case = case().unresolved_stars(false).known_failure("wrong source");
        let changes: &[fn(&mut StatementAnalysis)] = &[
            |actual| actual.output.columns[0].name = OutputName::Explicit(u("other")),
            |actual| actual.output.columns[0].ordinal = Some(9),
            |actual| actual.output.shape = Shape::Open { unexpanded: vec![] },
            |actual| actual.has_unresolved_stars = true,
            |actual| {
                actual.diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    kind: DiagnosticKind::EmptyProjection,
                    location: None,
                })
            },
            |actual| actual.output.columns.push(actual.output.columns[0].clone()),
        ];
        assert!(case.compare(&wrong_lineage()).is_ok());
        for change in changes {
            let mut actual = wrong_lineage();
            change(&mut actual);
            assert!(
                case.compare(&actual)
                    .unwrap_err()
                    .contains("expectation mismatch")
            );
        }
    }

    #[test]
    fn fixed_lineage_cannot_be_hidden_by_an_unrelated_mismatch() {
        let mut actual = statement();
        actual.output.columns[0].name = OutputName::Explicit(u("wrong_label"));
        assert!(
            case()
                .known_failure("wrong source")
                .compare(&actual)
                .unwrap_err()
                .contains("remove the known_failure flag")
        );
    }

    #[test]
    fn selected_column_failure_preserves_other_columns() {
        let mut case = case();
        case.expected.columns.push(col(1, u("second"), vec![]));
        let case = case.known_failure_at(1, "second source");
        let mut actual = statement();
        let mut second = actual.output.columns[0].clone();
        second.ordinal = Some(1);
        second.name = OutputName::Explicit(u("second"));
        second.lineage = complete(vec![source(relation("t"), u("wrong"))]);
        actual.output.columns.push(second);
        assert!(case.compare(&actual).is_ok());
        actual.output.columns[0].lineage = complete(vec![source(relation("t"), u("another"))]);
        assert!(case.compare(&actual).is_err());
        assert!(
            Case::new("invalid index", "SELECT 1", Dialect::Generic, vec![])
                .known_failure_at(1, "missing column")
                .run()
                .unwrap_err()
                .contains("setup error")
        );
    }

    fn merged_case() -> Case {
        Case::new(
            "merged",
            "SELECT * FROM a JOIN b USING (id)",
            Dialect::Generic,
            vec![
                col(
                    0,
                    u("id"),
                    vec![
                        source(relation("a"), u("id")),
                        source(relation("b"), u("id")),
                    ],
                ),
                col(
                    1,
                    u("left_value"),
                    vec![source(relation("a"), u("left_value"))],
                ),
                col(
                    2,
                    u("right_value"),
                    vec![source(relation("b"), u("right_value"))],
                ),
            ],
        )
        .unresolved_stars(false)
        .merged_star_failure(
            vec![
                source(relation("a"), u("id")),
                source(relation("b"), u("id")),
            ],
            "duplicate merged key",
        )
    }

    fn duplicated_key() -> StatementAnalysis {
        let mut actual = statement();
        actual.output.columns = [
            col(0, u("id"), vec![source(relation("a"), u("id"))]),
            col(
                1,
                u("left_value"),
                vec![source(relation("a"), u("left_value"))],
            ),
            col(2, u("id"), vec![source(relation("b"), u("id"))]),
            col(
                3,
                u("right_value"),
                vec![source(relation("b"), u("right_value"))],
            ),
        ]
        .into_iter()
        .map(|column| dlin_sqllineage::OutputColumn {
            ordinal: column.ordinal,
            name: column.name,
            lineage: column.lineage,
            transform: dlin_sqllineage::Transform::Direct,
        })
        .collect();
        actual
    }

    #[test]
    fn merged_star_failure_allows_only_duplicate_key_and_its_diagnostic() {
        let case = merged_case();
        let mut actual = duplicated_key();
        assert!(case.compare(&actual).is_ok());
        actual.diagnostics.push(Diagnostic {
            severity: Severity::Warning,
            kind: DiagnosticKind::DuplicateOutputName {
                name: u("id"),
                ordinals: vec![0, 2],
            },
            location: None,
        });
        assert!(case.compare(&actual).is_ok());
        let changes: &[fn(&mut StatementAnalysis)] = &[
            |actual| actual.output.columns[0].lineage = complete(vec![]),
            |actual| actual.output.columns[2].lineage = complete(vec![]),
            |actual| actual.output.columns[1].name = OutputName::Explicit(u("wrong")),
            |actual| actual.output.columns[1].lineage = complete(vec![]),
            |actual| actual.output.columns[1].ordinal = Some(9),
            |actual| {
                actual.output.columns.swap(1, 3);
                for (index, column) in actual.output.columns.iter_mut().enumerate() {
                    column.ordinal = Some(index);
                }
            },
            |actual| actual.output.shape = Shape::Open { unexpanded: vec![] },
            |actual| actual.has_unresolved_stars = true,
            |actual| {
                actual.diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    kind: DiagnosticKind::EmptyProjection,
                    location: None,
                })
            },
            |actual| {
                actual.diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    kind: DiagnosticKind::DuplicateOutputName {
                        name: u("left_value"),
                        ordinals: vec![1],
                    },
                    location: None,
                })
            },
        ];
        for change in changes {
            let mut changed = actual.clone();
            change(&mut changed);
            assert!(case.compare(&changed).is_err());
        }
    }

    #[test]
    fn fixed_star_bug_requires_flag_removal_despite_other_mismatches() {
        let mut actual = duplicated_key();
        actual.output.columns.remove(2);
        actual.output.columns[1].name = OutputName::Explicit(u("wrong"));
        assert!(
            merged_case()
                .compare(&actual)
                .unwrap_err()
                .contains("remove the known_failure flag")
        );
        let case = merged_case();
        let mut fixed = statement();
        fixed.output.columns = case
            .expected
            .columns
            .iter()
            .map(|column| dlin_sqllineage::OutputColumn {
                ordinal: column.ordinal,
                name: column.name.clone(),
                lineage: column.lineage.clone(),
                transform: dlin_sqllineage::Transform::Direct,
            })
            .collect();
        assert!(
            case.compare(&fixed)
                .unwrap_err()
                .contains("remove the known_failure flag")
        );
        let mut unflagged = case;
        unflagged.known_failure = None;
        assert!(unflagged.compare(&fixed).is_ok());
    }

    #[test]
    fn catalog_alias_returns_canonical_relation_for_independent_lookup_key() {
        let case = case()
            .catalog(vec![(relation("other"), vec![u("x")])])
            .catalog_alias(relation("raw"), relation("physical.Raw"), vec![q("ID")]);
        assert!(case.validate().is_ok());
        let catalog = case.catalog.as_ref().unwrap();
        let found = catalog.lookup(&relation("raw")).unwrap();
        assert_eq!(found.relation, relation("physical.Raw"));
        assert_eq!(found.columns[0].name, q("ID"));
        assert!(catalog.lookup(&relation("physical.Raw")).is_none());
        assert!(catalog.lookup(&relation("other")).is_some());
        assert!(
            case.catalog_alias(relation("raw"), relation("duplicate"), vec![])
                .validate()
                .unwrap_err()
                .contains("duplicate catalog relation")
        );
    }

    #[test]
    fn known_failure_does_not_hide_parse_or_statement_count_errors() {
        for (sql, expected_error) in [
            ("SELECT )", "parse error"),
            ("", "statement setup error"),
            ("SELECT 1; SELECT 2", "statement setup error"),
        ] {
            let mut case = case().known_failure("demonstration mismatch");
            case.sql = sql.into();
            assert!(case.run().unwrap_err().contains(expected_error));
        }
    }

    #[test]
    fn known_failure_does_not_hide_semantic_errors() {
        let mut case = case().known_failure("demonstration mismatch");
        case.sql = "SELECT a FROM t UNION ALL SELECT a, b FROM u".into();
        assert!(case.run().unwrap_err().contains("semantic error"));
        let mut actual = statement();
        actual.diagnostics.push(Diagnostic {
            severity: Severity::Error,
            kind: DiagnosticKind::UnexpectedCycle,
            location: None,
        });
        assert!(
            case.compare(&actual)
                .unwrap_err()
                .contains("semantic error")
        );
    }

    #[test]
    fn setup_errors_are_not_known_failures() {
        for reason in ["", "two\nlines"] {
            assert!(
                case()
                    .known_failure(reason)
                    .run()
                    .unwrap_err()
                    .contains("setup error")
            );
        }
        assert!(
            case()
                .catalog(vec![(relation("t"), vec![]), (relation("t"), vec![])])
                .known_failure("duplicate fixture")
                .run()
                .unwrap_err()
                .contains("setup error")
        );
    }

    #[test]
    fn comparison_checks_names_quotes_shape_and_diagnostics() {
        let mut actual = statement();
        actual.output.columns[0].name = OutputName::Explicit(q("value"));
        assert!(case().compare(&actual).is_err());
        let mut actual = statement();
        actual.output.shape = Shape::Open { unexpanded: vec![] };
        assert!(case().compare(&actual).is_err());
        let mut actual = statement();
        actual.diagnostics.push(Diagnostic {
            severity: Severity::Warning,
            kind: DiagnosticKind::EmptyProjection,
            location: None,
        });
        assert!(case().compare(&actual).is_err());
    }

    #[test]
    fn explicitly_expected_semantic_diagnostics_are_compared() {
        let diagnostic = Diagnostic {
            severity: Severity::Error,
            kind: DiagnosticKind::SetOperationArityMismatch { left: 1, right: 2 },
            location: None,
        };
        let case = Case::new(
            "arity",
            "SELECT a FROM t UNION ALL SELECT a, b FROM u",
            Dialect::Generic,
            vec![],
        )
        .diagnostics(vec![diagnostic.clone()]);
        assert!(case.run().is_ok());
        let mut actual = statement();
        actual.output.columns.clear();
        actual.diagnostics.push(diagnostic);
        assert!(case.compare(&actual).is_ok());
        actual.diagnostics.push(Diagnostic {
            severity: Severity::Error,
            kind: DiagnosticKind::UnexpectedCycle,
            location: None,
        });
        assert!(
            case.compare(&actual)
                .unwrap_err()
                .contains("semantic error")
        );
    }
}
