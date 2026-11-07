# dlin-sqllineage architecture and analysis contract

This document is the normative contract of the SQL analysis engine. It defines
what the engine promises about column lineage, in what terms, and under which
assumptions. Type documentation in the crate links here instead of defining the
terms again. The dbt-specific layer built on top of the engine is described in
the `dlin-core` architecture document, section "Column lineage".

Some of what follows describes the current behaviour and some describes the
agreed target. Section 10 states which, and the "Planned public types" section
lists the type changes that the target needs.

## Notation

Examples use PostgreSQL and no schema metadata unless they say otherwise.

| Notation | Meaning |
| --- | --- |
| `C[a.x]` | `Lineage::Complete([a.x])` |
| `P[K; U]` | `Lineage::Partial { known: K, unresolved: U }` |
| `Closed` | `Shape::Closed`: every output slot is enumerated. Independent of value-dependency completeness |
| `Open` | `Shape::Open`: some output segment is not enumerated |
| `FullOrdered t[x,y]` | A schema that lists every column of `t` and proves their order |
| `Partial t{x}` | A schema that proves only that `x` exists. Other columns are not proven absent, and the overall order is unknown |

Names such as `AlignmentUnknown` or `Unsupported(Lambda)` in examples refer to
the planned types described in sections 7 and 8 and in "Planned public types".
They do not all exist in the API today.

## 1. Assumptions and the kinds of dependency

**Valid SQL.** Apart from inputs that are rejected as errors, the engine assumes
the SQL is valid in the environment where it runs. It does not guess when the
information it has leaves a binding ambiguous. If a name could bind to more than
one thing, the engine does not pick one.

**Three kinds of dependency.** A query output can depend on its inputs in three
ways:

| Kind | Meaning | Example |
| --- | --- | --- |
| Value dependency | An input that supplies or computes the output value, by expression provenance | `t.x` in `SELECT t.x + 1`, and also `t.flag` in `CASE WHEN t.flag THEN ...` |
| Column use (role) | Where and in which role SQL uses a column | `t.y` used as a filter in `WHERE t.y > 0` |
| Row dependency | Dependence on row selection, existence, multiplicity, grouping or ordering | `COUNT(*)`, `EXISTS (...)`, `LIMIT` with `ORDER BY` |

The three are not mutually exclusive. One reference can belong to several of
them. For example, `t.y` in `WHERE t.y > 0` is a column use with the filter role
and also makes the row selection of `t` depend on `t`.

**Value dependency.** Value lineage follows the expressions that supply or
compute an output value, transitively through referenced output slots. It
includes operands and expression-level conditions, such as the condition and the
result branches of `CASE`. It is a static expression-provenance classification.
It does not claim to enumerate every input whose modification could change the
observed result.

**Row dependency.** Influences mediated by row selection, existence,
multiplicity, grouping or ordering are row dependencies, even when they change a
scalar result. A row-selection predicate does not become a value dependency
merely because the selected rows are consumed by an expression.

A scalar subquery contributes the value lineage of its selected output, while
its filtering and row-selection dependencies stay row dependencies. `IN`
contributes the value lineage of its left operand and of the compared subquery
output, and the predicates that select the subquery rows are row dependencies.
`EXISTS` observes row existence and does not consume the projected values of
its subquery.

| SQL | Value dependency | Row dependency |
| --- | --- | --- |
| `CASE WHEN t.flag THEN t.x ELSE t.y END` | `t.flag`, `t.x`, `t.y` | The row set of the enclosing query, kept separately |
| `(SELECT u.x FROM u WHERE u.b > 0)` | `u.x` | `u`, `u.b`. The predicate decides whether there are zero or one rows |
| `t.a IN (SELECT u.b FROM u WHERE u.c > 0)` | `t.a`, `u.b` | `u`, `u.c` |
| `EXISTS (SELECT 1 FROM u WHERE u.b > 0)` | none | `u`, `u.b` |
| `(SELECT SUM(u.x) FROM u WHERE u.b > 0)` | `u.x` | `u`, `u.b`. The predicate selects the aggregated rows |

**Dependency kinds are preserved when dependencies compose.** A dependency path
that passes through a row dependency is a row dependency at the final output.
For example, a column that is a row dependency of an upstream query stays one
downstream, even if the downstream query selects it as a plain column. A separate
path made only of value dependencies may establish a value dependency for the
same input and output, so one input can be both.

**Only value dependency decides Complete or Partial.** `Lineage` describes value
dependencies and nothing else. `Complete` therefore never means "all kinds of
dependency are resolved". Column use roles and row dependencies are separate
information. Until they are collected, they are reported as not collected
(`NotCollected`), never as an empty list, so that `Complete([])` cannot be read
as "has no dependency at all".

Cases: K1, Z2, R1, R2, R3, R4, V1, O1.

## 2. Identity

The engine keeps five kinds of identity apart.

| Thing | Identity |
| --- | --- |
| Name | An `Ident` with its original spelling, its quote style, and the dialect's normalization rule |
| Reference | One occurrence of a name in the SQL text. Its identity is its position, and it does not depend on what the reference can see (section 3) |
| Range variable | One occurrence of a relation in a FROM clause. Two occurrences of the same table are different range variables |
| Output slot | One output of a query. Identified by an id that is valid within one analysis result. Not the name, not the ordinal |
| Physical source | A `SourceColumn`: a relation key and a column key, compared by the dialect's SQL keys |

**Output slots.** Duplicate names, anonymous outputs and outputs with unknown
position are each kept as separate slots. Renames, forwarding through CTEs and
subqueries, and set operations connect slots explicitly. They do not match
slots again by name or ordinal. The id need not be stable across separate
analyses. It only has to distinguish outputs inside one result, including in
the JSON and cache forms that `dlin-core` stores.

**Physical sources.** Source lists are deduplicated and sorted by SQL keys, not
by display spelling. Two spellings that the dialect treats as the same relation
or column are one source. A quoted identifier is not the same as an unquoted
one when the dialect distinguishes them.

Cases: S1, S2, S3, S4, E3.

## 3. Binding and visibility

**Identity and visibility are separate.** A reference is one occurrence of a
name. What it means depends on the environment visible at the point where it is
written, not on the bindings its scope holds after the whole statement has been
built. Every reference therefore carries the environment visible at its own
position.

**One rule for relation names.** FROM items, `TABLE` queries and every other
syntax that names a relation bind through the same rule. A name qualified with a
schema or a catalog is always a physical relation and never binds to a CTE. An
unqualified name binds to a visible CTE of that name, and otherwise to a physical
relation. CTE names are a separate namespace: a range variable does not hide a
CTE with the same name, and does not take its place.

**Visibility by kind of scope.**

| Scope | What a reference inside it can see |
| --- | --- |
| Ordinary derived table | The environment of the enclosing query, but not the other items of its own FROM clause |
| `LATERAL` derived table, `APPLY` | The FROM items before it. Never a later item, and never the derived table being defined |
| Argument of a FROM item such as `UNNEST` | The FROM items before it. The alias being defined does not exist yet |
| Expression subquery (`IN`, `EXISTS`, scalar, `ARRAY(...)`) | The enclosing query as it stands at the position of the subquery |
| CTE | The ancestor environment allowed at its position (a CTE inside a `LATERAL` subquery can reference the outer FROM items before it). Within its own `WITH` list: only the CTEs defined before it, or, under `RECURSIVE`, every CTE of the list including itself and later ones (B4) |

**Structural walks share the scope rules.** Any walk that crosses query
boundaries, for example one that collects the relations a statement reads or the
bindings of a scope, uses the same scope rules as name resolution. No collector
leaks an inner binding or an inner output into the enclosing query (B3).

**Dialect extensions.** DuckDB lets a derived table see earlier FROM items
without writing `LATERAL`. That implicit lateral reference is a dialect
extension the engine does not model yet. A reference that depends on it is
reported as unresolved. It is never bound to a wrong object.

Cases: B1, B2, B3, B4, I5.

## 4. Evaluation context and cycles

Computing a lineage value can lead back to itself, for example through a
`LATERAL` item that reads its own output or through a recursive CTE.

**One evaluation context.** Every evaluation path shares one evaluation context:
the walk over the raw dependency graph, the computation of output slots, and name
resolution. The context keeps what is currently being evaluated apart from what
is already computed. A finished result is therefore never mistaken for a cycle,
and a result that is still being computed is never reused as an answer.

**Re-entry.** Reaching something that is already being evaluated, through any
path, is detected and reported as an unresolved value for the affected output. It
is never turned into an empty set and never recurses without end. Analysis does
not abort the process, whatever the SQL is (B2).

**Work budget.** The context may carry an optional bound on the work done for
one statement. Exhausting it behaves like reaching a cycle: evaluation stops and
the value is unresolved.

**Not the same as a trace boundary.** The `Cycle` terminal reason of `dlin-core`
(`TraceBoundary::Cycle`) is a different concept. It describes a traced path that
loops across models. The cycles in this section stay inside the analysis of one
statement.

Cases: B2.

## 5. Schema knowledge and evidence

**Partial schema.** A schema that lists some columns of a relation guarantees
that the listed columns exist. It does not prove that unlisted columns are
absent, and it does not give the order of all columns. Name resolution, star
expansion and NATURAL JOIN all use this one meaning.

**Coverage and order are independent.** A schema can be exhaustive (all column
names are known) while its physical order is unknown. In that case the shape can
be `Closed`, but ordinals of several columns, and positional correspondence in
set operations over them, cannot be established.

**Evidence.** Every piece of schema information the analysis relies on carries
evidence: why the engine believes it, how much it covers, and whether its order
is known. Recommended attributes:

| Attribute | Values or meaning |
| --- | --- |
| `origin` | SQL output, catalog artifact, or YAML declaration |
| `coverage` | `Exhaustive` or `Partial` |
| `coverage_basis` | Enumerated by the source, assumed by the YAML policy, or declared by an enforced contract |
| `order` | `Proven` or `Unknown` |
| `names` | Original spelling, whether it was quoted, and the matching rule applied |
| `subject` | The physical relation or the model id |
| `fingerprint` | Identifies the content and the analysis conditions |
| `observed_at` | Time of the artifact, if available; otherwise unknown |
| `derived_from` | Upstream evidence that an SQL-inferred schema used |

Rules:

- The engine does not infer physical column order from "all columns" alone. An
  order given by a YAML list may be kept as display order, but it is not proof
  of physical order.
- Output inferred from SQL and declarations from YAML are never merged into a
  union. They are stored separately and compared.
- A result that relied on assumed coverage keeps a reference to that evidence,
  so a consumer can tell which conclusions depend on the assumption. This
  covers every conclusion that used the assumption, including a conclusion that
  something is absent.
- A YAML name written without a quote flag is interpreted by the current
  matching rule, and the fact that no quoting was specified is kept.

How the dbt layer chooses these values for YAML is described in the `dlin-core`
architecture document.

Cases: P1, P2, P3, P4, E1, E2, E3, E4, E5.

## 6. known, Complete and Partial

**known.** `Partial.known` is the set of static value-dependency sources whose
correspondence to the target output does not change across all interpretations
that are consistent with the SQL and the adopted schema evidence. A source that
merely exists in some branch or candidate, without proof that it maps to this
output, is not in `known`. `known` does not mean the source is evaluated for
every row, and it does not mean the source always changes the result on real
data. Branches of `CASE` or `COALESCE` are static value dependencies and belong
in `known`.

**Complete.** `Complete(sources)` means every input column that the output
depends on by value was analysed, and `sources` is the result. `Complete([])`
means the analysis finished and found no input column. It does not say anything
about row dependencies or non-determinism. Complete means complete for this
value-lineage classification, not for all influences on the observable result.
Unsupported constructs, analysis failures and unresolved names must never be
turned into an empty set.

**Partial.** `Partial { known, unresolved }` has known sources and at least one
contribution that could not be resolved.

Normalization rules:

- Sources are deduplicated by the relation and column SQL keys.
- `Partial.unresolved` is never empty. With nothing unresolved, the value is
  `Complete`.
- If a source is confirmed through one path, it can stay in `known` even when
  another contribution is ambiguous.
- A conclusion that depends on YAML is a conclusion under the assumption that
  the YAML list is exhaustive, and it keeps its evidence.

Cases: K1, K2, K3, K4, Z1, Z2, Z3, Z4.

## 7. Shape, width, order and set-operation correspondence

**Shape.** `Closed` guarantees that all output slots are enumerated. It does not
guarantee that every ordinal is known. A schema that is complete but has unknown
order gives a `Closed` shape with `None` ordinals. `Open` means at least one
output segment is not enumerated, for example a star over a relation with an
unknown or partial schema. Each unknown segment keeps the reason it is unknown. Uncertainty about the order
is independent of enumeration and is not such a reason: it is a `Closed` shape
with `None` ordinals.

**Open is independent of Partial.** `Shape` says whether the list of outputs is
complete. `Lineage` says whether the value dependencies of one output are
complete. A `Closed` output can have a `Partial` lineage (K2), and an `Open`
shape can contain outputs with `Complete` lineage (P2).

**Ordinals.** An ordinal is assigned only when all earlier slots are
enumerated and ordered. After an unknown segment, later ordinals are `None`.

**Set operations.** Three things are kept apart: how output slots correspond,
which operand supplies values and which supplies only row dependencies, and the
shape of the result.

*Slot-local correspondence.* A positional set operation (`UNION`, `INTERSECT`,
`EXCEPT`) connects the i-th output of one side to the i-th output of the other.
Correspondence is established separately for each output slot, and it needs the
position to be proven in every relevant operand. An `Open` operand does not
prevent aligning a prefix whose positions are proven in every relevant operand.
Uncertainty after that prefix does not invalidate the correspondence already
established. For a slot whose correspondence is not provable, the contributions
of the side that cannot be placed are not put in `known`. They are kept as an
alignment-unknown unresolved contribution that lists the possible sources and
the operands that could not be inspected. This is different from an ordinary
name ambiguity.

An unproven correspondence affects only the dependency kind that needs it. For
example, an unknown right-side correspondence in `EXCEPT` creates a gap in row
dependencies. It does not by itself make the left-supplied value lineage
partial.

*Dependency rules per operator.* Value lineage and row dependencies are derived
separately.

| Operator | Supplies values to each output slot | Row dependencies |
| --- | --- | --- |
| `UNION ALL` | Both operands, for the corresponding slot | The row sets and multiplicities of both operands |
| `UNION DISTINCT` | Both operands | As above, plus duplicate elimination over the whole output row |
| `INTERSECT ALL` | Both operands | Matching over whole rows of both operands, and their multiplicities |
| `INTERSECT DISTINCT` | Both operands | Matching and duplicate elimination over whole rows of both operands |
| `EXCEPT ALL` | The left operand only | Exclusion over whole rows of both operands, and their multiplicities. The right operand contributes only row dependencies |
| `EXCEPT DISTINCT` | The left operand only | Exclusion and duplicate elimination over whole rows of both operands |

Treating both operands of `INTERSECT` as value suppliers is a choice of this
contract: a symmetric operation gets symmetric lineage, and the left operand is
not designated as the only supplier. `UNION` and `INTERSECT` without a
quantifier are `DISTINCT`.

Matching, exclusion and duplicate elimination compare complete aligned rows.
Their row dependencies therefore include every compared slot, not only the slot
whose lineage is being asked for. `ALL` does not remove row dependencies,
because `INTERSECT ALL` and `EXCEPT ALL` depend on the multiplicities of the
operands. Duplicate elimination adds row dependencies without changing which
operand supplies values. These are static provenance rules. They do not
describe which physical tuple an execution plan happens to keep (O1).

*Correspondence by name.* A `BY NAME` operation connects slots by name using the
dialect's matching rule. It does not need ordinals. The model keeps the
correspondence mode (strict, `INNER`, `FULL OUTER`, `LEFT OUTER`) and an
explicit column list (`ON (...)`). The output column set and order derive from
the mode and the list:

- Strict `BY NAME` without a mode or a column list: names and order follow the
  left operand (S4).
- `FULL OUTER`: the columns of the left operand, then the columns only the right
  operand has. An operand that lacks a column contributes a padded `NULL` slot
  with no input dependency (O2).
- `INNER` and `LEFT OUTER`: the columns the mode keeps, in the order of the left
  operand.
- `ON (...)`: the list fixes the columns and their order (O3).

A form the engine does not model is `Unsupported`. It is never processed as a
plain `BY NAME`.

Correspondence and dependency propagation are separate steps. `BY NAME` and its
mode first align the slots of both operands, padding missing columns with
`NULL` slots. The per-operator rules above then decide value and row
dependencies on the aligned slots. With `FULL OUTER UNION ALL BY NAME`, a
right-only column therefore takes its value from the right operand. With
`FULL OUTER EXCEPT DISTINCT BY NAME`, the same column takes its value from the
left operand's padded `NULL`, and the right operand's column is a row
dependency only (O5).

*Shape.* The shape of a set operation result is derived from the same slot model
as every other query, not computed separately from lineage. An inner set
operation can be `Open` while the final output stays `Closed` (K3).

`possible_sources` of an alignment-unknown contribution is not a monotone set
of proven facts. It may shrink or disappear when more schema information
arrives.

Cases: P2, P4, S2, S4, K3, K4, U2, U3, O1, O2, O3, O4, O5.

## 8. Lowering and Unsupported

Lowering is the step that turns SQL syntax into the engine's dependency model.
The rule is simple: **an unmodelled construct is never turned into "no
dependency".**

`Unsupported` means the SQL parsed successfully but the engine cannot yet model
the dependencies or the output structure of one construct. It is distinct from
four other things:

| Situation | Representation |
| --- | --- |
| Unknown schema (star over a relation without a catalog) | Open shape with an unknown segment for that relation, not `Unsupported` |
| Name ambiguity | `Ambiguous` |
| Set-operation correspondence cannot be proven | Alignment-unknown contribution |
| Parse failure | A failed input, not an empty or `C[]` output |

`Unsupported` propagates only to the outputs that are affected by the
unsupported construct. It does not spread unconditionally to other outputs of
the same statement (U1). If an unsupported construct affects only row
selection, such as part of a `WHERE`, the value lineage of directly projected
columns is not made `Partial`; the unsupported construct is still reported as a
diagnostic and as a gap in row dependencies.

An unsupported feature is identified by a stable code, separate from its
display text.

Lowering also has to give every expression and virtual source the same path to
dependencies. Subqueries in expressions (`ARRAY(SELECT ...)`, scalar subqueries,
`IN (SELECT ...)`) contribute the values they project, and the predicates inside
them contribute row dependencies. A subquery used only for existence (`EXISTS`)
contributes row dependencies, not value dependencies (R3). A table-valued
expression in a FROM clause, such as `UNNEST`, supplies the value lineage of its
arguments to the columns it produces (V1). Dependencies that exist or are
unresolved are never turned into an empty set. An argument built only from
constants, such as `UNNEST([1, 2])`, has no input dependency, and `C[]` is the
correct result for it.

Cases: Z3, Z4, U1, U2, U3, U4, R1, R2, R3, R4, V1, V2.

## 9. Invariants and their preconditions

Invariants hold only where the transformation preserves the SQL's bindings, the
correspondence between outputs, and the kind of the analysed relations. Results
are compared by corresponding output and by physical source identity, not by
display names.

**Refinement monotonicity.** Two kinds of change to the information the engine
relies on are kept apart.

- An evidence update that is consistent with the old information and only adds
  knowledge is a refinement, even if the artifact id or the fingerprint
  changes. What was established before as known or confirmed is preserved: under
  a refinement, the previous `known` of each output is contained in the new
  `known` (I1, E4).
- A change of assumption or policy, or a replacement by information that
  contradicts the old, is not a refinement. Results may change arbitrarily (E5).

Preservation is not unconditional even for an update that adds knowledge. Adding
a relation to the query can make an unqualified name ambiguous (I2), so it is not
a refinement.

**CTE and model split equivalence.** Writing a `WITH` clause in one model and
splitting the same `SELECT` into a separate dbt model give the same terminal
value lineage and completeness, provided dialect, column schema, aliases,
materialization and value semantics are the same. Model paths and the kind of
evidence are normalized before comparing (I3).

**Cache round-trip.** Storing an analysis and loading it back yields identical
information. After the evidence changes, a cache hit for the old entry is not
allowed (I4).

**Clause move does not leak scope.** Moving an expression from one clause to
another must not leak bindings or output slots to the outside. This does not
claim the same value lineage: moving a column from the projection into `WHERE`
changes its role. The compared objects are the bindings and output slots that
escape to the outer query (B3).

**Later-FROM non-interference.** Adding a later FROM item must not change how
earlier items resolve. This is checked at reference points whose visible
environment is preserved, such as inside an earlier `LATERAL` (I5). It is not
generalized to unqualified references in the final `SELECT`, whose environment
changes when a relation is added.

Cases: I1, I2, I3, I4, I5.

## 10. Contract cases

**Status: target.** The cases in this section state the agreed behaviour. Not
all of them pass today, and some describe behaviour that lands with the changes
described in sections 3 to 8. Each case has an ID. Tests refer to these IDs in
their names or comments, so a test can be traced back to the sentence it
checks. When a case changes, this table changes first.

Unless the table says otherwise: dialect is PostgreSQL, there is no schema
metadata, and the SQL is valid. "Complete" and "Partial" refer to value lineage
only.

### known

| ID | Dialect | SQL | Metadata | Expected output |
| --- | --- | --- | --- | --- |
| K1 | PostgreSQL | `SELECT CASE WHEN t.flag THEN t.x ELSE t.y END AS v FROM t` | none | `v` = `C[t.flag, t.x, t.y]`, Closed. Branch conditions are value dependencies. No extra evidence |
| K2 | PostgreSQL | `SELECT s.x + u.fee AS total FROM s CROSS JOIN (SELECT * FROM fees) u` | none | `total` = `P[[s.x]; [NamedBehindStar(fees, fee)]]`, Closed. The confirmed `s.x` is kept next to the unresolved contribution |
| K3 | PostgreSQL | `SELECT b FROM (SELECT * FROM t1 UNION ALL SELECT a, b FROM t2) s` | no catalog | `known` is empty. The unknown output of `t1` and the unprovable right-branch correspondence stay in `unresolved`. The final output is Closed and the inner set operation is Open. `t2.b` must not be in `known` |
| K4 | PostgreSQL | same as K3 | `FullOrdered t1[b,a]`, `t2[a,b]` | `b` = `C[t1.b, t2.a]`, Closed. Columns correspond by position, not by name. Catalog evidence is referenced |

For K3, the unresolved contribution has to be able to say "it cannot be proven
whether `t2.a` or `t2.b` corresponds on the right branch". That is not an
ordinary range-variable ambiguity. The unknown output of `t1` sits at the start
of the left branch, so no position of the right branch is proven, and `b` is not
provable either. O4 shows a case where the prefix before the unknown segment is
proven.

### Complete with an empty set

| ID | Dialect | SQL | Metadata | Expected output |
| --- | --- | --- | --- | --- |
| Z1 | PostgreSQL | `SELECT 1 AS n` | none | `n` = `C[]`, Closed. No column use and no row dependency |
| Z2 | PostgreSQL | `SELECT COUNT(*) AS n FROM t` | none | Value lineage `C[]`, Closed. Row dependency on `t`. Must not be shown as "no dependency at all" |
| Z3 | BigQuery | `SELECT ARRAY(SELECT x FROM t) AS xs` | none | `xs` = `C[t.x]`, Closed. Visiting the subquery must not leave `C[]` |
| Z4 | DuckDB | `SELECT list_transform(t.items, x -> x + t.bias) AS xs FROM t` | none, while lambda is unsupported | `xs` = `P[[t.items]; [Unsupported(Lambda)]]`, Closed. The unsupported lambda body is not an empty dependency. Once lambdas are supported, this case moves to a `Complete` result that includes `t.bias` |

### Partial schema

| ID | Dialect | SQL | Metadata | Expected output |
| --- | --- | --- | --- | --- |
| P1 | PostgreSQL | `SELECT t.x FROM t` | `Partial t{x}` | `x` = `C[t.x]`, Closed. Partial catalog evidence is referenced |
| P2 | PostgreSQL | `SELECT * FROM t` | `Partial t{x}` | Listed `x` = `C[t.x]`, `ordinal` is `None`. Shape is Open with an unknown segment `BehindStar(t)`. Not Closed just because `x` is listed |
| P3 | PostgreSQL | `SELECT x FROM a CROSS JOIN b` | `FullOrdered a[x]`, `Partial b{y}` | `x` = `P[[]; [Ambiguous(x, [a, b])]]`, Closed. That `b` does not list `x` is not proof that `b` has no `x` |
| P4 | PostgreSQL | `SELECT * FROM a NATURAL JOIN b` | `FullOrdered a[x]`, `Partial b{y}` | Open. It is not known whether `x` is a merged join key or an unmerged column. The result must not claim a known single column and must keep this uncertainty. It needs a schema-dependent unresolved contribution, which a plain unknown star cannot express |

### Schema evidence

| ID | Dialect | SQL | Metadata | Expected output |
| --- | --- | --- | --- | --- |
| E1 | PostgreSQL | `SELECT * FROM src` | No SQL definition, no catalog. YAML lists `[z, a]` | Closed. Columns `z`, `a` are listed, each lineage is the matching `src` column, both ordinals are unknown. Evidence: origin YAML, coverage `Exhaustive`, basis assumed by policy, order `Unknown` |
| E2 | PostgreSQL | upstream model: `SELECT z, a FROM raw.t` | YAML lists `[a, z, extra]` | The SQL schema provided downstream stays `[z, a]`. `extra` is not added to the SQL output. The difference between YAML and SQL can be reported as a diagnostic |
| E3 | Snowflake | `SELECT s."id" FROM src s` | YAML: `name: id`, `quote: true` | `id` = `C[src."id"]`, Closed. The YAML evidence keeps the quoting. It is not the same as the unquoted `id` |
| E4 | PostgreSQL | as E1 | The YAML evidence of E1 is replaced by catalog evidence `FullOrdered src[a, z]`, which agrees with the YAML columns | The order is analysed as `a, z`. The update is consistent with the old information and only adds the order, so it is a refinement: everything previously known stays known (I1). The fingerprint changed, so cached results of the old evidence are not reused |
| E5 | PostgreSQL | as E1 | The YAML evidence of E1 is replaced by catalog evidence `FullOrdered src[a, z, extra]` | The catalog contradicts the assumption that the YAML list is exhaustive. This is a replacement of the assumption, not a refinement. `SELECT *` now includes `extra`, and results may change in any way. Cached results of the old evidence are not reused |

### Slot identity

| ID | Dialect | SQL | Metadata | Expected output |
| --- | --- | --- | --- | --- |
| S1 | PostgreSQL | `SELECT t.a AS x, t.b AS x FROM t` | none | Two separate slots with ordinals 0 and 1, `C[t.a]` and `C[t.b]`, Closed. Lookup of `x` is `Duplicate` |
| S2 | PostgreSQL | `SELECT *, t.x FROM t` | no catalog | Open. The explicit `x` is its own slot with value `C[t.x]` and unknown ordinal. It is not merged with an `x` that might sit behind the unknown star |
| S3 | PostgreSQL | `WITH c(b, a) AS (SELECT a, b FROM t) SELECT a FROM c` | none | `a` = `C[t.b]`, Closed. A column alias list renames by position. It does not look up the source names again |
| S4 | BigQuery | `SELECT a, b FROM x UNION ALL BY NAME SELECT b, a FROM y` | none | `a` = `C[x.a, y.a]`, `b` = `C[x.b, y.b]`, Closed, ordinals 0 and 1 |

### Binding

| ID | Dialect | SQL | Metadata | Expected output |
| --- | --- | --- | --- | --- |
| B1 | PostgreSQL | `WITH c AS (SELECT x FROM t1) SELECT p.x FROM public.c AS p` | none | `x` = `C[public.c.x]`, Closed. `public.c` is a physical relation, not the CTE `c`, and the relations read include `public.c` |
| B2 | PostgreSQL | `SELECT d.x FROM t CROSS JOIN LATERAL (SELECT x) AS d` | catalog `t(x)` | `x` = `C[t.x]`. Analysis terminates normally. The inner `x` cannot see `d`, so `d` is not a candidate |
| B3 | BigQuery | `SELECT a FROM t WHERE ARRAY_LENGTH(ARRAY(SELECT b FROM u)) > 0` | none | One output `a` = `C[t.a]`, Closed. The binding and the output of the inner `SELECT b FROM u` do not leak into the outer query |
| B4 | PostgreSQL | `WITH RECURSIVE x AS (SELECT id FROM y), y AS (SELECT id FROM t) SELECT id FROM x` | none | `id` = `C[t.id]`. Under `RECURSIVE` the later CTE `y` is visible inside `x`, so `y` is never read as a physical relation. Without `RECURSIVE`, the same `y` inside `x` is the physical relation `y` |

### Set operations

| ID | Dialect | SQL | Metadata | Expected output |
| --- | --- | --- | --- | --- |
| O1 | PostgreSQL | `SELECT a FROM t1 EXCEPT SELECT b FROM t2` | none | `a` = `C[t1.a]`, Closed. Row dependencies on `t2.b`, and on `t1.a` for the comparison. `t2.b` is not in the value lineage |
| O2 | BigQuery | `SELECT x.a AS a FROM x FULL OUTER UNION ALL BY NAME SELECT y.b AS b FROM y` | none | Outputs `a` and `b`, in that order, Closed. `a` = `C[x.a]` and `b` = `C[y.b]`, each from the side that has the column |
| O3 | BigQuery | `SELECT a, b FROM x UNION ALL BY NAME ON (b, a) SELECT a, b FROM y` | none | Outputs in the order `b`, `a`, Closed. `b` = `C[x.b, y.b]` and `a` = `C[x.a, y.a]` |
| O4 | PostgreSQL | `SELECT t.id AS k, t.* FROM t UNION ALL SELECT u.id AS k, u.* FROM u` | no catalog | `k` = `C[t.id, u.id]`. Position 0 is proven in both operands, before the unknown segments. The shape is `Open` |
| O5 | BigQuery | `SELECT x.a AS a FROM x FULL OUTER EXCEPT DISTINCT BY NAME SELECT y.b AS b FROM y` | none | Outputs `a` and `b`. `a` = `C[x.a]`. `b` = `C[]`, the left operand's padded `NULL`. `y.b` (and the compared slots of both operands) are row dependencies only. Correspondence is decided first, then the `EXCEPT` rule applies |

### Value dependency, column use and row dependency

| ID | Dialect | SQL | Metadata | Expected output |
| --- | --- | --- | --- | --- |
| R1 | PostgreSQL | `SELECT t.x FROM t WHERE t.y > 0` | none | `x` = `C[t.x]`, Closed. Uses: `x` as projection, `y` as filter. Row dependency on `t` and `t.y` (row selection). `t.y` is not in the value lineage of `x` |
| R2 | BigQuery | `SELECT t.a IN (SELECT u.b FROM u) AS p FROM t` | none | `p` = `C[t.a, u.b]`, Closed. The right side of `IN` is not reduced to a plain read. The subquery has no predicate, so its row dependency is on `u` only |
| R3 | PostgreSQL | `SELECT EXISTS (SELECT 1 FROM u WHERE u.b > 0) AS p` | none | Value lineage of `p` is `C[]`, Closed. Row dependency on `u` and `u.b` |
| R4 | PostgreSQL | `SELECT CAST(t.x AS BIGINT) AS x FROM t` | none | Value lineage `C[t.x]`, Closed. The transform is classified as a cast in the target design |
| V1 | BigQuery | `SELECT item.x AS x FROM t CROSS JOIN UNNEST(t.items) AS item` | none | `x` has at least `t.items` in its value lineage, and is never `C[]`. The field `x` of the unnested element does not make a separate source |
| V2 | BigQuery | `SELECT item FROM UNNEST([1, 2]) AS item` | none | `item` = `C[]`. The argument is built only from constants, so the empty set is correct here |

R3 differs from R2 on purpose. `IN` compares the projected values of the right
side. `EXISTS` only tests whether rows exist and does not read the projected
value. In R1 and R3 the filter columns are row dependencies, as the dependency
table in section 1 states.

### Unsupported

| ID | Dialect | SQL | Metadata | Expected output |
| --- | --- | --- | --- | --- |
| U1 | DuckDB | `SELECT t.id, list_transform(t.items, x -> x + t.bias) AS xs FROM t` | none, lambda unsupported | `id` = `C[t.id]`, `xs` = `P[[t.items]; [Unsupported(Lambda)]]`, Closed. The unsupported construct does not affect `id` |
| U2 | PostgreSQL | `SELECT * FROM t` | no catalog | Open with `BehindStar(t)`. Not `Unsupported` |
| U3 | PostgreSQL | `SELECT x FROM s UNION ALL TABLE t` | none, before `TABLE` is modelled | Keeps the known `s.x` and adds `Unsupported(TableSource)`. `t` is included among the relations read. Once `TABLE t` is modelled like `SELECT * FROM t`, the unsupported marker goes away and the unknown-star reason applies when there is no catalog |
| U4 | PostgreSQL | `SELECT (` | none | Parse failure. Not a successful empty output and not `C[]` |

### Invariants

| ID | Dialect | Compared inputs | Expected relation |
| --- | --- | --- | --- |
| I1 | PostgreSQL | K3 without metadata, then K4 with the ordered catalog. Also E1, then E4 | Every previous `known` source is in the new `known`, whether or not the evidence id or fingerprint changed. A previous `known` containing `t2.b` is a failure |
| I2 | PostgreSQL | `SELECT x FROM a`, then `SELECT x FROM a CROSS JOIN b`, both with unknown schemas | The first may resolve `x` to `a`. The second is `Ambiguous`. Preserving `known` is not required, because adding a relation is not a refinement |
| I3 | PostgreSQL | `WITH m AS (SELECT z, a FROM raw.t) SELECT m.z FROM m`, and the same `SELECT` split into a separate model `m` | Same terminal value lineage and completeness, given the same dialect, column schema, aliases and value semantics. Model path and evidence kind are normalized before comparing |
| I4 | PostgreSQL | One analysis result before and after being stored in the cache, then after its evidence changes | The round trip preserves all information. A cache hit after an evidence change is not allowed |
| I5 | PostgreSQL | `SELECT d.x FROM t CROSS JOIN LATERAL (SELECT x) AS d`, then the same query with `CROSS JOIN u` appended after the `LATERAL` item. Catalog `t(x)`, `u(x)` | Both give `d.x` = `C[t.x]`. The inner `x` sees `t` only, so `u` is not a candidate and the name is not `Ambiguous`. The second query also reads `u` |

### Impact and terminal reasons

These cases involve model-level tracing. The engine supplies the lineage of each
model, and the `dlin-core` architecture document defines the terminal reasons
and exploration completeness used below.

| ID | SQL and models | Metadata | Expected output |
| --- | --- | --- | --- |
| T1 | model `m` = `SELECT t.x FROM ext.t`. `ext.t` is a declared source | none | Value lineage `C[ext.t.x]`, Closed. Terminal reason `ExternalRelation`. Complete within the declared exploration scope |
| T2 | model `m` = `SELECT 1 AS x`, model `d` = `SELECT x FROM m` | none | Terminal value dependency of `d.x` is `C[]`. The stop point `m.x` has reason `SourceFree`. `m.x` is not turned into an external source |
| T3 | model `s` = `SELECT x FROM raw.s`, model `p` = the partial result of K2 over `s`, model `d` = `SELECT total FROM p` | none | The known downstream columns of `s.x` include `p.total` and `d.total`, and both keep their `Partial` lineage. If the unknown part cannot be excluded, exploration is Incomplete |
| T4 | A downstream model reads a known `m.x`, but `m` failed analysis. Or the traced path has a cycle | none | `m.x` is kept as a boundary, with `AnalysisFailed` or `Cycle`. It is not called a terminal external source. Exploration is Incomplete |
| T5 | model `m` is in the manifest but has no compiled SQL. YAML lists its columns, or it has an enforced contract. model `d` = `SELECT x FROM m` | YAML columns or enforced contract | `m.x` is a boundary with reason `MissingDefinition`, not `ExternalRelation`. The schema declaration does not replace the definition. Exploration is Incomplete |
| T6 | `d` = `SELECT x FROM snap`, where `snap` is a snapshot whose materialization and history semantics are not modelled | none | `snap` is a boundary with reason `UnsupportedMaterialization`. Exploration is Incomplete. It is `ExternalRelation` only under an explicit boundary policy that is recorded in the result |

### Typed impact, cross-model order and assumptions

| ID | SQL and models | Metadata | Expected output |
| --- | --- | --- | --- |
| D1 | upstream model `first` = `SELECT z, a FROM raw_first`. downstream model = `SELECT * FROM first UNION ALL SELECT p, q FROM second` | `first` is analysed from SQL, so its order `z, a` is proven. `second` is a model with output `p, q` | Outputs `z` and `a`, in that order. `z` comes from `first.z` and `second.p`. `a` comes from `first.a` and `second.q`. The order proven inside `first` is preserved across the model boundary |
| D2 | source `raw_orders` with YAML listing only `id`. model = `SELECT * FROM raw_orders` | YAML only, default policy | The output lists `id`, with order unknown. Evidence `AssumedByYamlPolicy` is attached to the result and also to any impact or no-impact conclusion that used the assumption that there are no other columns |
| D3 | model `m` = `SELECT s.x FROM s WHERE s.y > 0`. model `d` = `SELECT x FROM m`. Input column `s.y` | none | See below |

For D3, the input `s.y` is a row dependency of `m.x`, and `m.x` is a plain value
dependency of `d.x`. Dependency kinds are preserved when they compose, so the
chain is a row dependency all the way.

| Requested scope | `confirmed` | `possible` | `exploration` |
| --- | --- | --- | --- |
| `ValueOnly` | empty | empty | Value: `Complete`. "No impact" holds for value dependencies only |
| `ValueAndRow`, row dependencies collected | `m.x` (Row), `d.x` (Row) | empty | Value: `Complete`. Row: `Complete` |
| `ValueAndRow`, row dependencies not collected | Row impact cannot be confirmed | empty. Nothing is invented because of the missing data | Value: `Complete`. Row: `Incomplete(NotCollected)` |

## Planned public types

The contract above needs several additions to the public types. They are the
design direction and land together with the changes that need them, not all at
once. Existing results and expectations may change with them.

**`Unresolved`.** The current variants stay. The following change or are added:

| Change | Purpose |
| --- | --- |
| `Unsupported { feature }` replaces `Unsupported { what }` | A stable feature code, separate from display text |
| `AlignmentUnknown { operation, possible_sources, unknown_operands }` | A set-operation correspondence that cannot be proven. Distinct from `Ambiguous` and `BehindStar`. `possible_sources` is not monotone: it can shrink or vanish with more schema information. `unknown_operands` is listed when not all candidates can be enumerated |
| `SchemaDependent { reason, candidates }` | Uncertainty caused by incomplete schema that is not a name ambiguity, for example whether a NATURAL JOIN column is merged |

**`OutputColumn`.** Gains an `id` that is valid within one analysis result and
an `evidence` list of the schema evidence the column depends on. The `ordinal`
stays optional.

**`Shape::Open`.** The current list of unexpanded stars generalizes to a list of
unknown output segments. Each segment has an id, a reason (unexpanded star,
`TABLE`, unsupported output, and so on), an optional relation,
its own `Lineage`, its evidence, and any known constraint on its position or
width. The current `UnexpandedStar` is one kind of segment. Reasons are about
enumeration only. Order uncertainty is not a reason, because a `Closed` shape
with `None` ordinals already expresses it.

**Set operations.** The lowered form of a set operation keeps the operator, the
quantifier (`ALL` or `DISTINCT`), and for `BY NAME` the correspondence mode and
the explicit column list. A form not covered is `Unsupported`.

**Schema evidence and catalog.** An evidence record with the attributes in
section 5, and catalog entries that state coverage, order and the evidence they
come from. The dbt layer owns the evidence content and passes the engine the
ids and the attributes needed to interpret them.

## Where this lives in the crate

| Area | Location |
| --- | --- |
| Output model, `Lineage`, `Unresolved`, `Shape` | `src/output.rs` |
| Catalog input | `src/catalog.rs` |
| Identifiers and SQL keys | `src/ident.rs`, `src/keys.rs` |
| Scope building and name resolution | `src/build/`, `src/resolve/` |
| Entry point | `src/analyzer.rs` |
