# Architecture

How sqllineage is structured and how to navigate the codebase.

## Overview

sqllineage takes a SQL string and produces, for each statement, a list of
input/output tables and a mapping from each output column to the source
columns it derives from. The analysis is purely syntactic — it does not
execute SQL or connect to a database. An optional `CatalogProvider` can be
supplied to resolve `SELECT *` and disambiguate unqualified columns.

The work happens in two phases:

1. **Build** (`build/`) — walk the AST produced by `sqlparser` and construct
   a dependency graph (`RawGraph`).
2. **Resolve** (`resolve/`) — traverse the graph, resolve column references
   through scope chains, and produce the final `AnalyzeResult`.

Each statement is processed independently with its own graph and scope tree.

## Build phase

`build/statement.rs` dispatches on the sqlparser `Statement` enum. Six
variants carry lineage (Query, Insert, CreateTable, Update, Delete, Merge);
all others produce `StatementType::Other`.

`build/query.rs` handles CTEs and set operations. CTEs are registered as
scope bindings in the parent scope; the query body runs in a child scope
to prevent CTE names from interfering with FROM bindings. UNION sides each
get their own scope; outputs are merged positionally. Recursive CTE
self-references are detected via `recursive_cte_name`, and edges from the
recursive step are marked with `is_recursive_back_edge`. CTE-wrapped DML
(`WITH ... UPDATE`, `WITH ... DELETE`, etc.) is handled via
`SetExpr::Update/Insert/Delete/Merge` delegation.

`build/select.rs` processes FROM first (registering bindings), then
projection (creating Output nodes). When a FROM item references a CTE,
the existing binding is reused — this requires a `lookup()` call during
Phase 1 to avoid adding CTE names to `tables.inputs`. Alias-less derived
tables are tracked separately on the scope because they have no name to
use as a binding key.

`build/expr.rs` matches all `Expr` variants exhaustively — no catch-all.
New variants added by sqlparser will cause a compile error, ensuring
lineage is never silently lost.

## Resolve phase

`resolve/mod.rs` iterates the root scope's output columns. Named columns
resolve via scope lookup: Table bindings produce `Concrete` origins,
CTE/DerivedTable bindings follow through `output_columns` to the base
table recursively. Star nodes go through the same chain via `expand_star()`.

Every resolve path handles all three binding types (Table, Cte, DerivedTable)
uniformly.

`resolve/topo.rs` validates that the graph is a DAG after removing recursive
CTE back-edges. `resolve/catalog.rs` applies the optional `CatalogProvider`
as a post-processing step.

## False positives

Two cases produce sources that may not contribute at runtime:

- **Conditional branches.** `CASE WHEN flag THEN a ELSE b END` includes
  both `a` and `b`. If `flag` is always true, `b` is a false positive.
- **Opaque functions.** `my_udf(a, b)` includes both arguments even if
  the function internally uses only `a`.

These are inherent to static analysis. Any other false positive is a bug.

## Limitations

- `SELECT *` and unqualified columns in multi-table scopes require a
  `CatalogProvider` for full resolution.
- `PIVOT`/`UNPIVOT` are not yet handled — output columns are generated
  dynamically from literal values, requiring semantic interpretation
  beyond AST traversal.

## Upgrading sqlparser

All sqlparser enums (`Statement`, `Expr`, `SetExpr`, `TableFactor`,
`SelectItem`) are matched exhaustively — no catch-all arms. New variants
will cause a compile error. For each new variant, determine whether it
carries column references (add traversal) or is lineage-neutral (return
empty / `StatementType::Other`).

## Module map

| Path | Responsibility |
|------|---------------|
| `lib.rs` | `pub fn analyze()` — parses SQL, runs build+resolve per statement |
| `types.rs` | Public types: `AnalyzeResult`, `TableRef`, `ColumnMapping`, etc. |
| `dialect.rs` | `Dialect` enum → sqlparser dialect mapping |
| `build/statement.rs` | Statement dispatch, UPDATE SET / MERGE WHEN column mapping |
| `build/query.rs` | CTE scope management, UNION merge, CTE-wrapped DML |
| `build/select.rs` | FROM binding registration, projection, CTE reference detection |
| `build/expr.rs` | Expression traversal for ancestor collection |
| `graph/node.rs` | `RawNode` enum: Output, Ref, Unqualified, Star |
| `graph/edge.rs` | `RawEdge` with `EdgeKind` and `is_recursive_back_edge` flag |
| `graph/scope.rs` | `ScopeTree`: bindings, output columns, anonymous derived tables |
| `resolve/mod.rs` | Scope-chain resolution, `expand_star()`, `ColumnMapping` assembly |
| `resolve/topo.rs` | Kahn's algorithm for DAG validation |
| `resolve/catalog.rs` | `CatalogProvider` application (Wildcard/Ambiguous refinement) |
