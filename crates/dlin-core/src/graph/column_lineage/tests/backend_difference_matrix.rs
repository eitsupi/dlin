//! Differential coverage for the two column-lineage backends.
//!
//! This is intentionally a test-local harness. Production continues to use the
//! backend selected by `compute_column_lineage`; this module constructs each
//! backend directly so a migration can be adjudicated before the switch.

use std::collections::{BTreeMap, BTreeSet};

use super::super::TransformationType;
use super::super::backend::{
    AnalysisCompleteness, AnalysisSlot, BackendAnalysis, BackendColumnOutcome, BackendError,
    BackendId, BackendSource, CatalogSnapshot, DlinDialect, LineageBackend, LineageRequest,
    OutputColumnRequest, ResolutionState, backend_for_tests, normalize_column_outcomes,
};

#[derive(Debug, Clone)]
struct MatrixCase {
    id: &'static str,
    sql: &'static str,
    dialect: DlinDialect,
    catalog: Option<CatalogSnapshot>,
    outputs: Vec<OutputSpec>,
}

#[derive(Debug, Clone, Copy)]
struct OutputSpec {
    slot: usize,
    name: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct CatalogTableSpec {
    name: &'static str,
    columns: &'static [&'static str],
}

fn catalog(tables: &[CatalogTableSpec]) -> CatalogSnapshot {
    let mut snapshot = CatalogSnapshot::new();
    for table in tables {
        snapshot.add_table(
            table.name,
            table.columns.iter().map(|column| (*column).to_string()),
        );
    }
    snapshot
}

fn case(
    id: &'static str,
    sql: &'static str,
    dialect: DlinDialect,
    catalog: Option<CatalogSnapshot>,
    outputs: &[OutputSpec],
) -> MatrixCase {
    MatrixCase {
        id,
        sql,
        dialect,
        catalog,
        outputs: outputs.to_vec(),
    }
}

fn generic_cases() -> Vec<MatrixCase> {
    let ext_a = || {
        Some(catalog(&[CatalogTableSpec {
            name: "ext_a",
            columns: &["col_x", "col_y"],
        }]))
    };

    vec![
        case(
            "plain_projection",
            "SELECT id, name FROM raw_table",
            DlinDialect::Generic,
            None,
            &[
                OutputSpec {
                    slot: 7,
                    name: "name",
                },
                OutputSpec {
                    slot: 2,
                    name: "id",
                },
            ],
        ),
        case(
            "aliased_projection",
            "SELECT id AS customer_id FROM raw_table",
            DlinDialect::Generic,
            None,
            &[OutputSpec {
                slot: 0,
                name: "customer_id",
            }],
        ),
        case(
            "expression",
            "SELECT amount + tax AS total FROM raw_table",
            DlinDialect::Generic,
            None,
            &[OutputSpec {
                slot: 0,
                name: "total",
            }],
        ),
        case(
            "aggregate",
            "SELECT COUNT(*) AS order_count FROM raw_table",
            DlinDialect::Generic,
            None,
            &[OutputSpec {
                slot: 0,
                name: "order_count",
            }],
        ),
        case(
            "case_expression",
            "SELECT CASE WHEN status = 'ok' THEN amount ELSE 0 END AS bucket FROM raw_table",
            DlinDialect::Generic,
            None,
            &[OutputSpec {
                slot: 0,
                name: "bucket",
            }],
        ),
        case(
            "cast",
            "SELECT CAST(amount AS INT) AS amount_int FROM raw_table",
            DlinDialect::Generic,
            None,
            &[OutputSpec {
                slot: 0,
                name: "amount_int",
            }],
        ),
        case(
            "cte",
            "WITH base AS (SELECT id AS order_id FROM raw_orders) SELECT order_id FROM base",
            DlinDialect::Generic,
            None,
            &[OutputSpec {
                slot: 0,
                name: "order_id",
            }],
        ),
        case(
            "nested_cte",
            "WITH base AS (SELECT id FROM raw_orders), renamed AS (SELECT id AS order_id FROM base) SELECT order_id FROM renamed",
            DlinDialect::Generic,
            None,
            &[OutputSpec {
                slot: 0,
                name: "order_id",
            }],
        ),
        case(
            "derived_table",
            "SELECT d.order_id FROM (SELECT id AS order_id FROM raw_orders) d",
            DlinDialect::Generic,
            None,
            &[OutputSpec {
                slot: 0,
                name: "order_id",
            }],
        ),
        case(
            "qualified_join_with_catalog",
            "SELECT o.id FROM orders o JOIN payments p ON o.id = p.order_id",
            DlinDialect::Generic,
            Some(catalog(&[
                CatalogTableSpec {
                    name: "orders",
                    columns: &["id", "customer_id"],
                },
                CatalogTableSpec {
                    name: "payments",
                    columns: &["order_id", "amount"],
                },
            ])),
            &[OutputSpec {
                slot: 0,
                name: "id",
            }],
        ),
        case(
            "unqualified_join_with_catalog",
            "SELECT id FROM orders o JOIN payments p ON o.id = p.order_id",
            DlinDialect::Generic,
            Some(catalog(&[
                CatalogTableSpec {
                    name: "orders",
                    columns: &["id", "customer_id"],
                },
                CatalogTableSpec {
                    name: "payments",
                    columns: &["order_id", "amount"],
                },
            ])),
            &[OutputSpec {
                slot: 0,
                name: "id",
            }],
        ),
        case(
            "qualified_join_without_catalog",
            "SELECT o.id FROM orders o JOIN payments p ON o.id = p.order_id",
            DlinDialect::Generic,
            None,
            &[OutputSpec {
                slot: 0,
                name: "id",
            }],
        ),
        case(
            "unqualified_join_without_catalog",
            "SELECT id FROM orders o JOIN payments p ON o.id = p.order_id",
            DlinDialect::Generic,
            None,
            &[OutputSpec {
                slot: 0,
                name: "id",
            }],
        ),
        case(
            "star_with_catalog",
            "SELECT * FROM ext_a",
            DlinDialect::Generic,
            ext_a(),
            &[OutputSpec {
                slot: 0,
                name: "col_x",
            }],
        ),
        case(
            "star_without_catalog",
            "SELECT * FROM ext_a",
            DlinDialect::Generic,
            None,
            &[OutputSpec {
                slot: 0,
                name: "col_x",
            }],
        ),
        case(
            "union_named_leading_operand",
            "SELECT col_a FROM left_t UNION ALL SELECT col_a FROM right_t",
            DlinDialect::Generic,
            None,
            &[OutputSpec {
                slot: 0,
                name: "col_a",
            }],
        ),
        case(
            "union_star_leading_operand",
            "SELECT * FROM ext_a UNION ALL SELECT 1 AS col_x",
            DlinDialect::Generic,
            ext_a(),
            &[OutputSpec {
                slot: 0,
                name: "col_x",
            }],
        ),
        case(
            "requested_output_missing",
            "SELECT id FROM raw_table",
            DlinDialect::Generic,
            None,
            &[OutputSpec {
                slot: 0,
                name: "missing",
            }],
        ),
        case(
            "unparseable_sql",
            "SELECT FROM raw_table",
            DlinDialect::Generic,
            None,
            &[OutputSpec {
                slot: 0,
                name: "id",
            }],
        ),
        case(
            "known_polyglot_overclaim",
            "WITH a AS (SELECT * FROM ext_a), u AS (SELECT 1 AS c1, 2 AS c2 UNION ALL SELECT a.col_x, a.col_y FROM a) SELECT c1 FROM u",
            DlinDialect::Generic,
            ext_a(),
            &[OutputSpec {
                slot: 0,
                name: "c1",
            }],
        ),
        case(
            "known_star_contribution_disappears",
            "WITH lit AS (SELECT 1 AS col_a), u AS (SELECT col_a FROM lit UNION ALL SELECT * FROM ext_a) SELECT col_a FROM u",
            DlinDialect::Generic,
            ext_a(),
            &[OutputSpec {
                slot: 0,
                name: "col_a",
            }],
        ),
    ]
}

fn identifier_cases() -> Vec<MatrixCase> {
    let catalog = || {
        Some(catalog(&[CatalogTableSpec {
            name: "RawTable",
            columns: &["OrderID", "MixedCol"],
        }]))
    };

    [DlinDialect::Snowflake, DlinDialect::BigQuery]
        .into_iter()
        .flat_map(|dialect| {
            let prefix = dialect.as_str();
            vec![
                case(
                    Box::leak(format!("{prefix}_qualified_mixed").into_boxed_str()),
                    "SELECT r.OrderID FROM RawTable r",
                    dialect,
                    catalog(),
                    &[OutputSpec {
                        slot: 0,
                        name: "OrderID",
                    }],
                ),
                case(
                    Box::leak(format!("{prefix}_unqualified_mixed").into_boxed_str()),
                    "SELECT OrderID FROM RawTable",
                    dialect,
                    catalog(),
                    &[OutputSpec {
                        slot: 0,
                        name: "orderid",
                    }],
                ),
                case(
                    Box::leak(format!("{prefix}_alias_case").into_boxed_str()),
                    "SELECT OrderID AS ResultCol FROM RawTable",
                    dialect,
                    catalog(),
                    &[OutputSpec {
                        slot: 0,
                        name: "resultcol",
                    }],
                ),
                case(
                    Box::leak(format!("{prefix}_quoted_identifier").into_boxed_str()),
                    "SELECT \"MixedCol\" AS quoted_result FROM \"RawTable\"",
                    dialect,
                    catalog(),
                    &[OutputSpec {
                        slot: 0,
                        name: "quoted_result",
                    }],
                ),
            ]
        })
        .collect()
}

fn corpus() -> Vec<MatrixCase> {
    generic_cases()
        .into_iter()
        .chain(identifier_cases())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CanonicalRun {
    result: Result<Vec<CanonicalStatement>, CanonicalError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CanonicalError {
    kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CanonicalStatement {
    lineage_bearing: bool,
    has_unresolved_stars: bool,
    completeness: String,
    columns: BTreeMap<usize, CanonicalOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CanonicalOutcome {
    Resolved {
        resolution: ResolutionState,
        transformation: TransformationType,
        sources: Vec<String>,
    },
    Failed {
        resolution: ResolutionState,
    },
}

fn canonical_source(source: &BackendSource) -> String {
    // Source order is not semantic. These tagged, escaped strings preserve every
    // source variant and field while providing a total order for sorting.
    match source {
        BackendSource::Concrete { table, column } => {
            format!("concrete(table={table:?},column={column:?})")
        }
        BackendSource::Ambiguous { column, candidates } => {
            let mut candidates = candidates.clone();
            candidates.sort();
            format!("ambiguous(column={column:?},candidates={candidates:?})")
        }
        BackendSource::Wildcard { table } => format!("wildcard(table={table:?})"),
        BackendSource::Recursive { base_sources } => {
            let mut base_sources = base_sources
                .iter()
                .map(canonical_source)
                .collect::<Vec<_>>();
            base_sources.sort();
            format!("recursive(base_sources={base_sources:?})")
        }
    }
}

fn canonical_completeness(completeness: &AnalysisCompleteness) -> String {
    match completeness {
        AnalysisCompleteness::Complete => "Complete".to_string(),
        AnalysisCompleteness::Indeterminate { reason } => {
            format!("Indeterminate(reason={reason:?})")
        }
    }
}

fn canonical_analysis(analysis: BackendAnalysis, outputs: &[OutputColumnRequest]) -> CanonicalRun {
    CanonicalRun {
        result: Ok(analysis
            .statements
            .into_iter()
            .map(|statement| {
                let (outcomes, diagnostics) = normalize_column_outcomes(outputs, statement.columns);
                let columns = outcomes
                    .into_iter()
                    .map(|outcome| match outcome {
                        BackendColumnOutcome::Resolved(result) => (
                            result.target.slot.0,
                            CanonicalOutcome::Resolved {
                                resolution: result.resolution,
                                transformation: result.transformation,
                                sources: {
                                    let mut sources = result
                                        .sources
                                        .iter()
                                        .map(canonical_source)
                                        .collect::<Vec<_>>();
                                    sources.sort();
                                    sources
                                },
                            },
                        ),
                        BackendColumnOutcome::Failed(failure) => (
                            failure.target.slot.0,
                            CanonicalOutcome::Failed {
                                resolution: failure.resolution,
                            },
                        ),
                    })
                    .collect::<BTreeMap<_, _>>();
                assert_contract_diagnostics(&diagnostics, statement.statement_ordinal);
                CanonicalStatement {
                    lineage_bearing: statement.lineage_bearing,
                    has_unresolved_stars: statement.has_unresolved_stars,
                    completeness: canonical_completeness(&statement.completeness),
                    columns,
                }
            })
            .collect()),
    }
}

fn assert_contract_diagnostics(diagnostics: &[BackendError], statement_ordinal: usize) {
    assert!(
        diagnostics.is_empty(),
        "backend contract violation in statement {statement_ordinal}: {diagnostics:?}"
    );
}

fn run(case: &MatrixCase, backend_id: BackendId) -> CanonicalRun {
    let outputs = case
        .outputs
        .iter()
        .map(|output| OutputColumnRequest {
            slot: AnalysisSlot(output.slot),
            name: output.name.to_string(),
        })
        .collect::<Vec<_>>();
    let duplicate_output_names = BTreeSet::new();
    let request = LineageRequest {
        sql: case.sql,
        dialect: case.dialect,
        catalog: case.catalog.as_ref(),
        outputs: &outputs,
        duplicate_output_names: &duplicate_output_names,
    };

    match backend_for_tests(backend_id).analyze(&request) {
        Ok(analysis) => canonical_analysis(analysis, &outputs),
        Err(error) => CanonicalRun {
            result: Err(CanonicalError {
                kind: format!("{:?}", error.kind),
            }),
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Difference {
    case_id: &'static str,
    field: String,
    polyglot: String,
    sqllineage: String,
}

fn differences(
    case: &MatrixCase,
    polyglot: &CanonicalRun,
    sqllineage: &CanonicalRun,
) -> Vec<Difference> {
    let mut differences = Vec::new();
    match (&polyglot.result, &sqllineage.result) {
        (Err(polyglot), Err(sqllineage)) => {
            if polyglot.kind != sqllineage.kind {
                differences.push(Difference {
                    case_id: case.id,
                    field: "analysis.error.kind".to_string(),
                    polyglot: polyglot.kind.clone(),
                    sqllineage: sqllineage.kind.clone(),
                });
            }
        }
        (Err(polyglot), Ok(sqllineage)) => differences.push(Difference {
            case_id: case.id,
            field: "analysis.shape".to_string(),
            polyglot: format!("error({})", polyglot.kind),
            sqllineage: format!("{} statement(s)", sqllineage.len()),
        }),
        (Ok(polyglot), Err(sqllineage)) => differences.push(Difference {
            case_id: case.id,
            field: "analysis.shape".to_string(),
            polyglot: format!("{} statement(s)", polyglot.len()),
            sqllineage: format!("error({})", sqllineage.kind),
        }),
        (Ok(polyglot), Ok(sqllineage)) => {
            if polyglot.len() != sqllineage.len() {
                differences.push(Difference {
                    case_id: case.id,
                    field: "statement.count".to_string(),
                    polyglot: polyglot.len().to_string(),
                    sqllineage: sqllineage.len().to_string(),
                });
            }
            for (ordinal, (polyglot, sqllineage)) in
                polyglot.iter().zip(sqllineage.iter()).enumerate()
            {
                compare_field(
                    &mut differences,
                    case.id,
                    ordinal,
                    "lineage_bearing",
                    polyglot.lineage_bearing,
                    sqllineage.lineage_bearing,
                );
                compare_field(
                    &mut differences,
                    case.id,
                    ordinal,
                    "has_unresolved_stars",
                    polyglot.has_unresolved_stars,
                    sqllineage.has_unresolved_stars,
                );
                compare_field(
                    &mut differences,
                    case.id,
                    ordinal,
                    "completeness",
                    &polyglot.completeness,
                    &sqllineage.completeness,
                );

                let slots = polyglot
                    .columns
                    .keys()
                    .chain(sqllineage.columns.keys())
                    .copied()
                    .collect::<BTreeSet<_>>();
                for slot in slots {
                    match (polyglot.columns.get(&slot), sqllineage.columns.get(&slot)) {
                        (Some(polyglot), Some(sqllineage)) => compare_outcome(
                            &mut differences,
                            case.id,
                            ordinal,
                            slot,
                            polyglot,
                            sqllineage,
                        ),
                        (polyglot, sqllineage) => differences.push(Difference {
                            case_id: case.id,
                            field: format!("statement[{ordinal}].column[{slot}].presence"),
                            polyglot: format!("{}", polyglot.is_some()),
                            sqllineage: format!("{}", sqllineage.is_some()),
                        }),
                    }
                }
            }
        }
    }
    differences
}

fn compare_outcome(
    differences: &mut Vec<Difference>,
    case_id: &'static str,
    ordinal: usize,
    slot: usize,
    polyglot: &CanonicalOutcome,
    sqllineage: &CanonicalOutcome,
) {
    match (polyglot, sqllineage) {
        (
            CanonicalOutcome::Resolved {
                resolution: _polyglot_resolution,
                transformation: polyglot_transformation,
                sources: polyglot_sources,
            },
            CanonicalOutcome::Resolved {
                resolution: _sqllineage_resolution,
                transformation: sqllineage_transformation,
                sources: sqllineage_sources,
            },
        ) => {
            compare_column_field(
                differences,
                case_id,
                ordinal,
                slot,
                "transformation",
                polyglot_transformation,
                sqllineage_transformation,
            );
            compare_column_field(
                differences,
                case_id,
                ordinal,
                slot,
                "sources",
                polyglot_sources,
                sqllineage_sources,
            );
        }
        (
            CanonicalOutcome::Failed {
                resolution: polyglot,
            },
            CanonicalOutcome::Failed {
                resolution: sqllineage,
            },
        ) => compare_column_field(
            differences,
            case_id,
            ordinal,
            slot,
            "resolution",
            polyglot,
            sqllineage,
        ),
        (polyglot, sqllineage) => differences.push(Difference {
            case_id,
            field: format!("statement[{ordinal}].column[{slot}].outcome_kind"),
            polyglot: outcome_kind(polyglot).to_string(),
            sqllineage: outcome_kind(sqllineage).to_string(),
        }),
    }

    compare_column_field(
        differences,
        case_id,
        ordinal,
        slot,
        "resolution",
        resolution_of(polyglot),
        resolution_of(sqllineage),
    );
}

fn resolution_of(outcome: &CanonicalOutcome) -> &ResolutionState {
    match outcome {
        CanonicalOutcome::Resolved { resolution, .. } | CanonicalOutcome::Failed { resolution } => {
            resolution
        }
    }
}

fn outcome_kind(outcome: &CanonicalOutcome) -> &'static str {
    match outcome {
        CanonicalOutcome::Resolved { .. } => "resolved",
        CanonicalOutcome::Failed { .. } => "failed",
    }
}

fn compare_column_field<T: std::fmt::Debug + PartialEq>(
    differences: &mut Vec<Difference>,
    case_id: &'static str,
    ordinal: usize,
    slot: usize,
    name: &str,
    polyglot: &T,
    sqllineage: &T,
) {
    if polyglot != sqllineage {
        differences.push(Difference {
            case_id,
            field: format!("statement[{ordinal}].column[{slot}].{name}"),
            polyglot: format!("{polyglot:?}"),
            sqllineage: format!("{sqllineage:?}"),
        });
    }
}

fn compare_field<T: std::fmt::Debug + PartialEq>(
    differences: &mut Vec<Difference>,
    case_id: &'static str,
    ordinal: usize,
    name: &str,
    polyglot: T,
    sqllineage: T,
) {
    if polyglot != sqllineage {
        differences.push(Difference {
            case_id,
            field: format!("statement[{ordinal}].{name}"),
            polyglot: format!("{polyglot:?}"),
            sqllineage: format!("{sqllineage:?}"),
        });
    }
}

enum LedgerStatus {
    Decided { verdict: &'static str },
    Open { to_settle: &'static str },
}

struct LedgerEntry {
    case_id: &'static str,
    field: &'static str,
    polyglot: &'static str,
    sqllineage: &'static str,
    status: LedgerStatus,
    authority: &'static str,
    must_observe: bool,
}

// Keep this table deliberately explicit. A new backend difference must carry
// its two observed values, a status-specific verdict or open question, and an
// observation policy.
const LEDGER: &[LedgerEntry] = &[
    LedgerEntry {
        case_id: "cast",
        field: "statement[0].column[0].transformation",
        polyglot: "Cast",
        sqllineage: "Direct",
        status: LedgerStatus::Decided {
            verdict: "Accepted. sqllineage classifies a cast as a passthrough, and dlin accepted that when it dropped its own `Cast` transformation for this migration. Column lineage is an experimental feature and the loss was taken deliberately.",
        },
        authority: "dlin transformation contract and the backend implementation notes",
        must_observe: true,
    },
    LedgerEntry {
        case_id: "aggregate",
        field: "statement[0].column[0].transformation",
        polyglot: "Aggregation",
        sqllineage: "Direct",
        status: LedgerStatus::Open {
            to_settle: "For COUNT(*), settle whether the transformation should be Direct as sqllineage reports or Aggregation as polyglot reports. Sqllineage reports Aggregation for aggregates that take a column argument, so this appears specific to aggregates with no column argument and needs confirming upstream.",
        },
        authority: "dlin backend comparison",
        must_observe: true,
    },
    LedgerEntry {
        case_id: "aggregate",
        field: "statement[0].column[0].sources",
        polyglot: "[\"concrete(table=\\\"\\\",column=\\\"order_count\\\")\"]",
        sqllineage: "[]",
        status: LedgerStatus::Open {
            to_settle: "For COUNT(*), settle whether dlin should emit no sources, as sqllineage does because COUNT(*) reads no column, or the polyglot source with an empty table name naming the output column, which is a fabrication.",
        },
        authority: "dlin backend comparison",
        must_observe: true,
    },
    LedgerEntry {
        case_id: "unqualified_join_without_catalog",
        field: "statement[0].column[0].outcome_kind",
        polyglot: "resolved",
        sqllineage: "failed",
        status: LedgerStatus::Open {
            to_settle: "Settle whether an unqualified id in a join with no catalog should be resolved, as polyglot does, or rejected, as sqllineage does; the corpus does not establish which behavior dlin should publish.",
        },
        authority: "dlin backend comparison",
        must_observe: true,
    },
    LedgerEntry {
        case_id: "unqualified_join_without_catalog",
        field: "statement[0].column[0].resolution",
        polyglot: "Resolved",
        sqllineage: "Ambiguous",
        status: LedgerStatus::Open {
            to_settle: "Settle whether the resolution state for an unqualified id in a join with no catalog should be Resolved, as polyglot reports, or Ambiguous, as sqllineage reports; the corpus does not establish which state dlin should publish.",
        },
        authority: "dlin backend comparison",
        must_observe: true,
    },
    LedgerEntry {
        case_id: "star_with_catalog",
        field: "statement[0].has_unresolved_stars",
        polyglot: "true",
        sqllineage: "false",
        status: LedgerStatus::Open {
            to_settle: "Settle whether catalog expansion of SELECT * should clear the unresolved-star marker, as sqllineage reports, or retain it, as polyglot reports; the observed values do not establish which metadata dlin should publish.",
        },
        authority: "dlin backend comparison",
        must_observe: true,
    },
    LedgerEntry {
        case_id: "star_without_catalog",
        field: "statement[0].column[0].resolution",
        polyglot: "NotFound",
        sqllineage: "Indeterminate",
        status: LedgerStatus::Decided {
            verdict: "Accepted. The two backends label the same inability differently, NotFound against Indeterminate, and dlin's own rule is that an unresolvable source makes lineage indeterminate rather than the output absent. The sqllineage label follows that rule.",
        },
        authority: "dlin backend comparison",
        must_observe: true,
    },
    LedgerEntry {
        case_id: "union_named_leading_operand",
        field: "statement[0].column[0].transformation",
        polyglot: "Unknown",
        sqllineage: "Direct",
        status: LedgerStatus::Open {
            to_settle: "Settle whether a set operation with named leading operands should classify its output as Unknown, as polyglot reports, or Direct, as sqllineage reports; the corpus does not establish which transformation dlin should publish.",
        },
        authority: "dlin backend comparison",
        must_observe: true,
    },
    LedgerEntry {
        case_id: "union_star_leading_operand",
        field: "statement[0].completeness",
        polyglot: "\"Complete\"",
        sqllineage: "\"Indeterminate(reason=\\\"a set operation whose leading branch is SELECT * cannot be aligned with its other branches, so lineage for this statement cannot be trusted\\\")\"",
        status: LedgerStatus::Decided {
            verdict: "sqllineage is right. A set operation whose leading branch is `SELECT *` cannot be aligned with its other branches, so refusing is correct and polyglot reporting a complete, resolved result is an overclaim. This is the behavior dlin's guard exists to produce.",
        },
        authority: "sqllineage safety guard and dlin comparison",
        must_observe: true,
    },
    LedgerEntry {
        case_id: "union_star_leading_operand",
        field: "statement[0].column[0].outcome_kind",
        polyglot: "resolved",
        sqllineage: "failed",
        status: LedgerStatus::Decided {
            verdict: "sqllineage is right. A set operation whose leading branch is `SELECT *` cannot be aligned with its other branches, so refusing is correct and polyglot reporting a complete, resolved result is an overclaim. This is the behavior dlin's guard exists to produce.",
        },
        authority: "sqllineage safety guard and dlin comparison",
        must_observe: true,
    },
    LedgerEntry {
        case_id: "union_star_leading_operand",
        field: "statement[0].column[0].resolution",
        polyglot: "Resolved",
        sqllineage: "Indeterminate",
        status: LedgerStatus::Decided {
            verdict: "sqllineage is right. A set operation whose leading branch is `SELECT *` cannot be aligned with its other branches, so refusing is correct and polyglot reporting a complete, resolved result is an overclaim. This is the behavior dlin's guard exists to produce.",
        },
        authority: "sqllineage safety guard and dlin comparison",
        must_observe: true,
    },
    LedgerEntry {
        case_id: "known_polyglot_overclaim",
        field: "statement[0].column[0].outcome_kind",
        polyglot: "resolved",
        sqllineage: "failed",
        status: LedgerStatus::Open {
            to_settle: "This is a finding, not a preference: polyglot overclaims the mixed set-operation lineage and sqllineage is incomplete.",
        },
        authority: "column-lineage review finding; neither backend",
        must_observe: true,
    },
    LedgerEntry {
        case_id: "known_polyglot_overclaim",
        field: "statement[0].column[0].resolution",
        polyglot: "Resolved",
        sqllineage: "Indeterminate",
        status: LedgerStatus::Open {
            to_settle: "Neither backend is right for this known finding: polyglot overclaims and sqllineage is incomplete. The resolution states make that distinction explicit.",
        },
        authority: "column-lineage review finding; neither backend",
        must_observe: true,
    },
    LedgerEntry {
        case_id: "known_star_contribution_disappears",
        field: "statement[0].column[0].transformation",
        polyglot: "Unknown",
        sqllineage: "Direct",
        status: LedgerStatus::Open {
            to_settle: "Sqllineage resolves the literal branch as Direct while the polyglot set-operation result is Unknown; record the observed result.",
        },
        authority: "column-lineage review finding; neither backend",
        must_observe: true,
    },
    LedgerEntry {
        case_id: "known_star_contribution_disappears",
        field: "statement[0].column[0].sources",
        polyglot: "[\"concrete(table=\\\"\\\",column=\\\"col_a\\\")\", \"concrete(table=\\\"ext_a\\\",column=\\\"*\\\")\"]",
        sqllineage: "[]",
        status: LedgerStatus::Open {
            to_settle: "Sqllineage reports a resolved constant with no sources, so the star branch contribution disappears without a trace; this is a finding, not an accepted preference.",
        },
        authority: "column-lineage review finding; neither backend",
        must_observe: true,
    },
    LedgerEntry {
        case_id: "snowflake_qualified_mixed",
        field: "statement[0].column[0].sources",
        polyglot: "[\"concrete(table=\\\"RawTable\\\",column=\\\"OrderID\\\")\"]",
        sqllineage: "[\"concrete(table=\\\"rawtable\\\",column=\\\"OrderID\\\")\"]",
        status: LedgerStatus::Open {
            to_settle: "Settle which spelling dlin should publish in ColumnSource.table: polyglot's catalog spelling RawTable or sqllineage's lowercased spelling rawtable. This is public output that changes what users see, and it must be settled before the production backend changes.",
        },
        authority: "identifier-normalization regression coverage",
        must_observe: true,
    },
    LedgerEntry {
        case_id: "snowflake_unqualified_mixed",
        field: "statement[0].column[0].sources",
        polyglot: "[\"concrete(table=\\\"RawTable\\\",column=\\\"OrderID\\\")\"]",
        sqllineage: "[\"concrete(table=\\\"rawtable\\\",column=\\\"OrderID\\\")\"]",
        status: LedgerStatus::Open {
            to_settle: "Settle which spelling dlin should publish in ColumnSource.table: polyglot's catalog spelling RawTable or sqllineage's lowercased spelling rawtable. This is public output that changes what users see, and it must be settled before the production backend changes.",
        },
        authority: "identifier-normalization regression coverage",
        must_observe: true,
    },
    LedgerEntry {
        case_id: "snowflake_alias_case",
        field: "statement[0].column[0].sources",
        polyglot: "[\"concrete(table=\\\"RawTable\\\",column=\\\"OrderID\\\")\"]",
        sqllineage: "[\"concrete(table=\\\"rawtable\\\",column=\\\"OrderID\\\")\"]",
        status: LedgerStatus::Open {
            to_settle: "Settle which spelling dlin should publish in ColumnSource.table: polyglot's catalog spelling RawTable or sqllineage's lowercased spelling rawtable. This is public output that changes what users see, and it must be settled before the production backend changes.",
        },
        authority: "identifier-normalization regression coverage",
        must_observe: true,
    },
    LedgerEntry {
        case_id: "bigquery_qualified_mixed",
        field: "statement[0].column[0].sources",
        polyglot: "[\"concrete(table=\\\"RawTable\\\",column=\\\"OrderID\\\")\"]",
        sqllineage: "[\"concrete(table=\\\"rawtable\\\",column=\\\"OrderID\\\")\"]",
        status: LedgerStatus::Open {
            to_settle: "Settle which spelling dlin should publish in ColumnSource.table: polyglot's catalog spelling RawTable or sqllineage's lowercased spelling rawtable. This is public output that changes what users see, and it must be settled before the production backend changes.",
        },
        authority: "identifier-normalization regression coverage",
        must_observe: true,
    },
    LedgerEntry {
        case_id: "bigquery_unqualified_mixed",
        field: "statement[0].column[0].sources",
        polyglot: "[\"concrete(table=\\\"RawTable\\\",column=\\\"OrderID\\\")\"]",
        sqllineage: "[\"concrete(table=\\\"rawtable\\\",column=\\\"OrderID\\\")\"]",
        status: LedgerStatus::Open {
            to_settle: "Settle which spelling dlin should publish in ColumnSource.table: polyglot's catalog spelling RawTable or sqllineage's lowercased spelling rawtable. This is public output that changes what users see, and it must be settled before the production backend changes.",
        },
        authority: "identifier-normalization regression coverage",
        must_observe: true,
    },
    LedgerEntry {
        case_id: "bigquery_alias_case",
        field: "statement[0].column[0].sources",
        polyglot: "[\"concrete(table=\\\"RawTable\\\",column=\\\"OrderID\\\")\"]",
        sqllineage: "[\"concrete(table=\\\"rawtable\\\",column=\\\"OrderID\\\")\"]",
        status: LedgerStatus::Open {
            to_settle: "Settle which spelling dlin should publish in ColumnSource.table: polyglot's catalog spelling RawTable or sqllineage's lowercased spelling rawtable. This is public output that changes what users see, and it must be settled before the production backend changes.",
        },
        authority: "identifier-normalization regression coverage",
        must_observe: true,
    },
    LedgerEntry {
        case_id: "bigquery_quoted_identifier",
        field: "statement[0].column[0].transformation",
        polyglot: "Unknown",
        sqllineage: "Direct",
        status: LedgerStatus::Open {
            to_settle: "Settle whether the BigQuery quoted-identifier projection should be classified as Unknown, as polyglot reports, or Direct, as sqllineage reports. The corpus shows the classification difference but does not establish why it occurs or which behavior dlin should publish.",
        },
        authority: "dlin backend comparison",
        must_observe: true,
    },
    LedgerEntry {
        case_id: "bigquery_quoted_identifier",
        field: "statement[0].column[0].sources",
        polyglot: "[\"concrete(table=\\\"\\\",column=\\\"quoted_result\\\")\"]",
        sqllineage: "[]",
        status: LedgerStatus::Open {
            to_settle: "Settle whether the BigQuery quoted-identifier projection should emit no sources, as sqllineage reports, or the concrete source with an empty table name and the output column as its column, as polyglot reports. The corpus shows the difference but does not establish why it occurs or which source behavior dlin should publish.",
        },
        authority: "dlin backend comparison",
        must_observe: true,
    },
];

fn ledger_entry(case_id: &str, field: &str) -> Option<&'static LedgerEntry> {
    LEDGER
        .iter()
        .find(|entry| entry.case_id == case_id && entry.field == field)
}

#[test]
fn backend_difference_matrix_is_adjudicated() {
    let cases = corpus();
    let mut observed = BTreeSet::new();
    let mut unledgered = Vec::new();
    let mut stale = Vec::new();

    for case in &cases {
        let polyglot = run(case, BackendId::Polyglot);
        let sqllineage = run(case, BackendId::Sqllineage);
        for difference in differences(case, &polyglot, &sqllineage) {
            let key = (difference.case_id, difference.field.clone());
            if let Some(entry) = ledger_entry(difference.case_id, &difference.field) {
                observed.insert(key);
                if entry.polyglot != difference.polyglot
                    || entry.sqllineage != difference.sqllineage
                {
                    stale.push(format!(
                        "case '{}' field '{}': ledger recorded polyglot={} sqllineage={}, observed polyglot={} sqllineage={}",
                        difference.case_id,
                        difference.field,
                        entry.polyglot,
                        entry.sqllineage,
                        difference.polyglot,
                        difference.sqllineage
                    ));
                }
            } else {
                unledgered.push(format!(
                    "case '{}' field '{}': polyglot={} sqllineage={}",
                    difference.case_id,
                    difference.field,
                    difference.polyglot,
                    difference.sqllineage
                ));
            }
        }
    }

    let missing = LEDGER
        .iter()
        .filter(|entry| {
            entry.must_observe && !observed.contains(&(entry.case_id, entry.field.to_string()))
        })
        .map(|entry| format!("case '{}' field '{}'", entry.case_id, entry.field))
        .collect::<Vec<_>>();
    let stale_shape = LEDGER
        .iter()
        .filter(|entry| {
            !entry.must_observe && !observed.contains(&(entry.case_id, entry.field.to_string()))
        })
        .map(|entry| format!("case '{}' field '{}'", entry.case_id, entry.field))
        .collect::<Vec<_>>();

    assert!(
        unledgered.is_empty(),
        "unledgered backend difference(s):\n{}",
        unledgered.join("\n")
    );
    assert!(
        stale.is_empty(),
        "ledger entry's difference no longer has the recorded shape:\n{}",
        stale.join("\n")
    );
    assert!(
        stale_shape.is_empty(),
        "ledger entry's difference no longer has the recorded shape (no longer observed):\n{}",
        stale_shape.join("\n")
    );
    assert!(
        missing.is_empty(),
        "must-observe ledger entry was never exercised:\n{}",
        missing.join("\n")
    );

    // Keep the status metadata live: every ledger entry must carry the required
    // payload for its status and state who made it, even entries that are not
    // currently required to be exercised.
    for entry in LEDGER {
        match &entry.status {
            LedgerStatus::Decided { verdict } => {
                assert!(!verdict.is_empty(), "ledger verdict is empty");
            }
            LedgerStatus::Open { to_settle } => {
                assert!(!to_settle.is_empty(), "ledger open question is empty");
            }
        }
        assert!(!entry.authority.is_empty(), "ledger authority is empty");
    }
}

#[test]
fn backend_difference_matrix_reports_open_findings() {
    let mut findings = Vec::new();

    for case in corpus() {
        let polyglot = run(&case, BackendId::Polyglot);
        let sqllineage = run(&case, BackendId::Sqllineage);
        for difference in differences(&case, &polyglot, &sqllineage) {
            let Some(entry) = ledger_entry(difference.case_id, &difference.field) else {
                continue;
            };
            let LedgerStatus::Open { to_settle } = &entry.status else {
                continue;
            };
            findings.push(format!(
                "case '{}' field '{}': polyglot={} sqllineage={} — to settle: {}",
                difference.case_id,
                difference.field,
                difference.polyglot,
                difference.sqllineage,
                to_settle
            ));
        }
    }

    println!("open backend difference findings ({}):", findings.len());
    for finding in findings {
        println!("- {finding}");
    }
}
