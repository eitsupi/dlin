# dlin-sqllineage architecture and analysis contract

This document is the normative contract of the SQL analysis engine. It defines
what the engine promises about column lineage, in what terms, and under which
assumptions. Type documentation in the crate links here instead of defining the
terms again. The dbt-specific layer built on top of the engine is described in
the `dlin-core` architecture document, section "Column lineage".

Some of what follows describes the current behaviour and some describes the
agreed target. Section 8 states which, and the "Planned public types" subsection
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
the planned types described at the end of section 6 and in "Planned public
types". They do not all exist in the API today.

## 1. Assumptions and the kinds of dependency

**Valid SQL.** Apart from inputs that are rejected as errors, the engine assumes
the SQL is valid in the environment where it runs. It does not guess when the
information it has leaves a binding ambiguous. If a name could bind to more than
one thing, the engine does not pick one.

**Three kinds of dependency.** A query output can depend on its inputs in three
ways:

| Kind | Meaning | Example |
| --- | --- | --- |
| Value dependency | An input that determines the value of the output expression | `t.x` in `SELECT t.x + 1`, and also `t.flag` in `CASE WHEN t.flag THEN ...` |
| Column use (role) | Where and in which role SQL uses a column | `t.y` used as a filter in `WHERE t.y > 0` |
| Row dependency | Dependence on the row set, row count, existence or row order | `COUNT(*)`, `EXISTS (...)`, `LIMIT` with `ORDER BY` |

The three are not mutually exclusive. One reference can belong to several of
them. For example, `t.y` in `WHERE t.y > 0` is a column use with the filter role
and also makes the row selection of `t` depend on `t`.

**Only value dependency decides Complete or Partial.** `Lineage` describes value
dependencies and nothing else. `Complete` therefore never means "all kinds of
dependency are resolved". Column use roles and row dependencies are separate
information. Until they are collected, they are reported as not collected
(`NotCollected`), never as an empty list, so that `Complete([])` cannot be read
as "has no dependency at all".

## 2. Identity

The engine keeps four kinds of identity apart.

| Thing | Identity |
| --- | --- |
| Name | An `Ident` with its original spelling, its quote style, and the dialect's normalization rule |
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

## 3. Schema knowledge and evidence

**Partial schema.** A schema that lists some columns of a relation guarantees
that the listed columns exist. It does not prove that unlisted columns are
absent, and it does not give the order of all columns. Name resolution, star
expansion and NATURAL JOIN all use this one meaning.

**Coverage and order are independent.** A schema can be exhaustive (all column
names are known) while its physical order is unknown. In that case the shape can
be `Closed`, but ordinals of several columns, and positional correspondence in
set operations, cannot be established.

**Evidence.** Every piece of schema information the analysis relies on carries
evidence: why the engine believes it, how much it covers, and whether its order
is known. Recommended attributes:

| Attribute | Values or meaning |
| --- | --- |
| `origin` | SQL output, catalog artifact, or YAML declaration |
| `coverage` | `Exhaustive` or `Partial` |
| `coverage_basis` | Enumerated by the source, or assumed by the YAML policy |
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
  so a consumer can tell which conclusions depend on the assumption.
- A YAML name written without a quote flag is interpreted by the current
  matching rule, and the fact that no quoting was specified is kept.

How the dbt layer chooses these values for YAML is described in the `dlin-core`
architecture document.

Cases: P1, P2, P3, P4, E1, E2, E3, E4.

## 4. known, Complete and Partial

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
about row dependencies or non-determinism. Unsupported constructs, analysis
failures and unresolved names must never be turned into an empty set.

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

## 5. Shape, width, order and set-operation correspondence

**Shape.** `Closed` guarantees that all output slots are enumerated. It does not
guarantee that every ordinal is known. A schema that is complete but has unknown
order gives a `Closed` shape with `None` ordinals. `Open` means at least one
output segment is not enumerated, for example a star over a relation with an
unknown or partial schema. Each unknown segment keeps the reason it is unknown.

**Open is independent of Partial.** `Shape` says whether the list of outputs is
complete. `Lineage` says whether the value dependencies of one output are
complete. A `Closed` output can have a `Partial` lineage (K2), and an `Open`
shape can contain outputs with `Complete` lineage (P2).

**Ordinals.** An ordinal is assigned only when all earlier slots are
enumerated and ordered. After an unknown segment, later ordinals are `None`.

**Set operations.**

- A positional set operation (`UNION`, `INTERSECT`, `EXCEPT`) can connect the
  i-th output of one side to the i-th output of the other only when both
  positions are proven. If either side is `Open`, or has unknown order, the
  correspondence is not provable. Then the right-side contributions are not
  put in `known`. They are kept as an alignment-unknown unresolved
  contribution that lists the possible sources and the operands that could not
  be inspected. This is different from an ordinary name ambiguity.
- A `BY NAME` operation connects slots by name using the dialect's matching
  rule. It does not need ordinals. The result follows the left operand for its
  names and order (S4).
- The shape of the set operation result is derived from the same slot model as
  every other query, not computed separately from lineage. An inner set
  operation can be `Open` while the final output stays `Closed` (K3).

`possible_sources` of an alignment-unknown contribution is not a monotone set
of proven facts. It may shrink or disappear when more schema information
arrives.

Cases: P2, P4, S2, S4, K3, K4, U2, U3.

## 6. Lowering and Unsupported

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
dependencies. Subqueries in expressions (`ARRAY(SELECT ...)`, `IN (SELECT ...)`)
contribute the values they project. A subquery used only for existence
(`EXISTS`) contributes row dependencies, not value dependencies (R3).

Cases: Z3, Z4, U1, U2, U3, U4, R1, R2, R3, R4.

## 7. Invariants and their preconditions

Invariants hold only where the transformation preserves the SQL's bindings, the
correspondence between outputs, and the kind of the analysed relations. Results
are compared by corresponding output and by physical source identity, not by
display names.

**Refinement monotonicity.** A refinement is an addition of schema information
for relations already in the query, under the same adoption policy, that does
not contradict earlier facts or assumptions. Under a refinement, the previous
`known` of each output is contained in the new `known` (I1). This is not
unconditional. Adding a relation to the query can make an unqualified name
ambiguous (I2), so it is not a refinement. Replacing an assumption or a piece
of evidence (E4) is also not a refinement, and it may change the result
arbitrarily.

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
escape to the outer query.

**Later-FROM non-interference.** Adding a later FROM item must not change how
earlier items resolve. This is checked at reference points whose visible
environment is preserved, such as inside an earlier `LATERAL`. It is not
generalized to unqualified references in the final `SELECT`, whose environment
changes when a relation is added.

Cases: I1, I2, I3, I4.

## 8. Contract cases

**Status: target.** The cases in this section state the agreed behaviour. Not
all of them pass today, and some describe behaviour that lands with the changes
described in sections 3 to 6. Each case has an ID. Tests refer to these IDs in
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
ordinary range-variable ambiguity.

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
| E4 | PostgreSQL | as E1 | The YAML evidence of E1 is replaced by catalog evidence `FullOrdered src[a, z]` | The order is analysed as `a, z`. This is a replacement of the assumption, not a refinement, so the old ordinals need not be kept. Cached results are invalidated |

### Slot identity

| ID | Dialect | SQL | Metadata | Expected output |
| --- | --- | --- | --- | --- |
| S1 | PostgreSQL | `SELECT t.a AS x, t.b AS x FROM t` | none | Two separate slots with ordinals 0 and 1, `C[t.a]` and `C[t.b]`, Closed. Lookup of `x` is `Duplicate` |
| S2 | PostgreSQL | `SELECT *, t.x FROM t` | no catalog | Open. The explicit `x` is its own slot with value `C[t.x]` and unknown ordinal. It is not merged with an `x` that might sit behind the unknown star |
| S3 | PostgreSQL | `WITH c(b, a) AS (SELECT a, b FROM t) SELECT a FROM c` | none | `a` = `C[t.b]`, Closed. A column alias list renames by position. It does not look up the source names again |
| S4 | BigQuery | `SELECT a, b FROM x UNION ALL BY NAME SELECT b, a FROM y` | none | `a` = `C[x.a, y.a]`, `b` = `C[x.b, y.b]`, Closed, ordinals 0 and 1 |

### Value dependency, column use and row dependency

| ID | Dialect | SQL | Metadata | Expected output |
| --- | --- | --- | --- | --- |
| R1 | PostgreSQL | `SELECT t.x FROM t WHERE t.y > 0` | none | `x` = `C[t.x]`, Closed. Uses: `x` as projection, `y` as filter. The row selection depends on `t` and `y` |
| R2 | BigQuery | `SELECT t.a IN (SELECT u.b FROM u) AS p FROM t` | none | `p` = `C[t.a, u.b]`, Closed. The right side of `IN` is not reduced to a plain read |
| R3 | PostgreSQL | `SELECT EXISTS (SELECT 1 FROM u WHERE u.b > 0) AS p` | none | Value lineage of `p` is `C[]`, Closed. The existence and row dependency belongs to `u` and the condition on `b` |
| R4 | PostgreSQL | `SELECT CAST(t.x AS BIGINT) AS x FROM t` | none | Source `C[t.x]`, Closed. The transform is classified as a cast in the target design |

R3 differs from R2 on purpose. `IN` compares the projected values of the right
side. `EXISTS` only tests whether rows exist and does not read the projected
value.

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
| I1 | PostgreSQL | K3 without metadata, then K4 with the ordered catalog | Every previous `known` source is in the new `known`. A previous `known` containing `t2.b` is a failure |
| I2 | PostgreSQL | `SELECT x FROM a`, then `SELECT x FROM a CROSS JOIN b`, both with unknown schemas | The first may resolve `x` to `a`. The second is `Ambiguous`. Preserving `known` is not required, because adding a relation is not a refinement |
| I3 | PostgreSQL | `WITH m AS (SELECT z, a FROM raw.t) SELECT m.z FROM m`, and the same `SELECT` split into a separate model `m` | Same terminal value lineage and completeness, given the same dialect, column schema, aliases and value semantics. Model path and evidence kind are normalized before comparing |
| I4 | PostgreSQL | One analysis result before and after being stored in the cache, then after its evidence changes | The round trip preserves all information. A cache hit after an evidence change is not allowed |

### Impact and terminal reasons

These cases involve model-level tracing. The engine supplies the lineage of each
model, and the `dlin-core` architecture document defines the terminal reasons
and exploration completeness used below.

| ID | SQL and models | Metadata | Expected output |
| --- | --- | --- | --- |
| T1 | model `m` = `SELECT t.x FROM ext.t`. `ext.t` is a declared source with no model definition | none | Value lineage `C[ext.t.x]`, Closed. Terminal reason `ExternalRelation`. Complete within the declared exploration scope |
| T2 | model `m` = `SELECT 1 AS x`, model `d` = `SELECT x FROM m` | none | Terminal value dependency of `d.x` is `C[]`. The stop point `m.x` has reason `SourceFree`. `m.x` is not turned into an external source |
| T3 | model `s` = `SELECT x FROM raw.s`, model `p` = the partial result of K2 over `s`, model `d` = `SELECT total FROM p` | none | The known downstream columns of `s.x` include `p.total` and `d.total`, and both keep their `Partial` lineage. If the unknown part cannot be excluded, exploration is Incomplete |
| T4 | A downstream model reads a known `m.x`, but `m` failed analysis. Or the traced path has a cycle | none | `m.x` is kept as a boundary, with `AnalysisFailed` or `Cycle`. It is not called a terminal external source. Exploration is Incomplete |

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
`TABLE`, unsupported output, unknown order, and so on), an optional relation,
its own `Lineage`, its evidence, and any known constraint on its position or
width. The current `UnexpandedStar` is one kind of segment.

**Schema evidence and catalog.** An evidence record with the attributes in
section 3, and catalog entries that state coverage, order and the evidence they
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
