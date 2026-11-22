# dlin-core architecture

## Column lineage

This section describes how `dlin-core` applies the SQL analysis engine to a dbt
project. It is the target design that the column lineage code is being changed
to match. Parts of it are not implemented yet, and where the current code
differs, the code is expected to move toward this description.

The meaning of the terms used here (value dependency, known, Complete,
Partial, Shape, output slot, schema evidence, Unsupported) is defined once, in
the engine contract: [`dlin-sqllineage` architecture](../dlin-sqllineage/ARCHITECTURE.md).
This section does not repeat those definitions. It covers what is specific to
dbt: where schema information comes from, how analyses are shared and cached,
how lineage crosses model boundaries, and how impact results are reported.

Code lives under `src/graph/column_lineage/`. The contract cases (K3, E1, T3
and so on) that tests refer to are listed in section 10 of the engine contract.

### YAML policy

A model or source can declare columns in YAML. When no better schema exists, the
declaration is used so that `SELECT *` can be expanded.

| Rule | Behaviour |
| --- | --- |
| Default coverage | A YAML column list with no SQL definition and no catalog is treated as exhaustive |
| Recorded as an assumption | The evidence records `coverage = Exhaustive` with basis "assumed by the YAML policy", so a result can be traced back to the assumption. It does not mean the columns were verified in the warehouse |
| Order | Always `Unknown`. The YAML array order is kept for display, but it is not proof of physical order |
| Positional set operations | A star over an upstream that has only YAML columns cannot be matched by position. `UNION` and similar operations over it give an alignment-unknown result (see engine contract, section 5), not a guess |
| SQL output and YAML | The columns inferred from SQL and the columns declared in YAML are kept as separate records. They are compared, never merged into a union. A column that is in YAML but not in the SQL output is not added to what the model provides downstream, and the difference can be reported as a diagnostic |
| Quoting | A YAML name keeps its original spelling and whether quoting was specified |
| Strict mode | A mode that does not assume YAML lists are exhaustive may be added later. The default stays as above |

The default is a product decision: it makes `SELECT *` over YAML-only relations
useful, at the price of an assumption that the results must show.

**Assumption visibility.** Public results must identify the adopted evidence on
which a conclusion depends. This applies to value-lineage completeness,
confirmed impact paths, exploration completeness and no-impact conclusions.
Evidence for absence or exhaustive coverage is attached to the conclusion that
used it, not only to the listed output columns. For example, "no other column
depends on this input" can rest on the assumption that no unlisted column
exists, and the evidence of that assumption belongs to the no-impact conclusion.

**Coverage basis.** The basis of exhaustive coverage is recorded, and the values
are not interchangeable:

| Basis | Meaning |
| --- | --- |
| `AssumedByYamlPolicy` | The YAML list is treated as exhaustive because the default policy says so |
| `DeclaredEnforcedContract` | A supported dbt model contract declared with `contract: {enforced: true}` lists the columns |

An enforced contract has a basis of its own, distinct from the default YAML
assumption. The declaration alone is not evidence that a particular warehouse
state was built or validated. Whether enforcement applies, and any validation
artifact that is available, are recorded separately. An enforced contract does
not prove the physical column order by itself.

Cases: E1, E2, E3, E4, E5, D2.

### ModelAnalysis

Today the same model SQL can be analysed more than once under the same
conditions: once to discover output names, once to provide a schema to
downstream models, and once more during cross-model tracing. The target is one
`ModelAnalysis` per model and set of conditions, shared by all three.

A `ModelAnalysis` is the public, serializable result for one model:

| Field | Content |
| --- | --- |
| `model_id` | The dbt model |
| `dialect` | The dialect used |
| `output` | Output slots with order, duplicates and quoting preserved, plus the shape |
| `evidence` | The schema evidence the analysis adopted |
| `diagnostics` | Diagnostics for the model |
| `relations` | Column uses and row dependencies, each either not collected or collected with listed gaps |

The relation fields use a collection type with an explicit "not collected" state,
so an uncollected dependency kind is never read as an empty one.

Because discovery, schema provision and tracing read the same analysis, they
cannot disagree about the output of a model. A model that fails to parse fails
the same way for all three.

**Analysis boundary.** A `ModelAnalysis` records one model's SQL analysis under a
specific dialect, schema evidence and analysis policy. Its source references are
the relations that the model's SQL names. They are not replaced by the terminal
sources of other models. Resolving those references to dbt nodes, and tracing
through them, are separate derived operations on top of the analysis.

### Output slots

The `output` of a model result is `OutputSlots<S>`: the slots in SQL order and
the shape. `S` is the representation of a source column, and it is the only
difference between the public and the internal result.

```text
OutputSlots<S> { columns: [OutputSlot<S>], shape: OutputShape }
OutputSlot<S>  { id, ordinal, name: OutputName, transformation, lineage: ValueLineage<S> }
OutputName     = Explicit(Ident) | Inferred(Ident) | Anonymous
OutputShape    = Closed | Open { unexpanded: [UnexpandedStar { relation, after_ordinal }] }
```

- The slots come from the engine's output columns, one for one. Nothing is
  sorted or deduplicated by name, an anonymous expression is a slot without a
  name, and an unexpanded star is a shape entry, not a slot.
- `id` is the index of the slot in the result it belongs to. It is not a
  persistent identity, and it is separate from `ordinal`, the SQL position that
  the engine proved (`None` after an unexpanded star).
- A name keeps its spelling and quote style (`Ident`). Names are compared only
  through the dialect's column key, never by their text. A lookup by name gives
  `Unique(slot)`, `Ambiguous(slots)` or `Absent`.
- `output` is `None` when the SQL could not be analysed (no compiled SQL, a
  parse failure, more than one statement). The model diagnostics say why. A
  failure is never a closed output without columns.
- The counts of the public result are computed from the slots when it is built.
  An unexpanded star is not a slot, so it is not counted.
- Columns that the YAML declares and the SQL output does not list are not
  added. They are non-fatal diagnostics: a closed output "does not contain"
  the column, while an open output only "does not enumerate" it, because an
  unexpanded star may still provide it.
- `ModelOutputRef` identifies a slot by the unique id of its model and the slot
  id, and carries the display names. Diagnostics that are about one slot carry
  its id too, so columns with the same name stay apart.

### Evidence and cache keys

Schema evidence is part of what an analysis result depends on. Each cache layer
is keyed by the inputs of that layer:

| Cache | Identity includes |
| --- | --- |
| SQL analysis | The SQL, the dialect and normalization settings, the adopted evidence (by evidence fingerprint), and the analysis policy (for example the YAML coverage policy) |
| Node resolution and tracing | Everything above, plus the inputs that resolve relations to dbt nodes and the boundary policy |
| A cache that combines layers | The union of the inputs of the layers it combines |

When any of it changes, for example when YAML evidence is replaced by catalog
evidence (E4, E5), the cached entry is invalid and a hit is not allowed. A change
only in the relation-to-node inputs invalidates the node-resolution and tracing
results without forcing the pure SQL analysis to run again.

The package version is the sole cache compatibility boundary; there is no
separate analysis-semantics identifier. Inputs that can vary within a package
version, such as adopted evidence and analysis policy, remain part of the
relevant cache key.

Stored results keep their evidence references, so loading an entry gives back
the same information that was stored (I4).

**Evidence in results.** Each piece of schema evidence has an id: `yaml:` and
the unique id for a YAML declaration, `sql:` and the unique id for a model's SQL
output, `user:` and the relation for a user-typed schema.

- A model result cites evidence ids where the engine reported them: each known
  source and unresolved contribution, each slot's structure and lineage as a
  whole, and the output's shape. Cross-model tracing composes them along the
  path: a source reached through an upstream model also relies on what the
  reference to that model relied on, and what decided the upstream output's
  value is part of the root's lineage. A trace boundary cites what the trace
  relied on to reach it. Its classification follows the manifest and the
  boundary policy, not schema evidence.
- A model's SQL output schema records the evidence it was derived from: what
  decided which columns the output lists, their names and positions, and
  whether it has more. What only a column's value relied on is not part of it.
  A reader of the model relies on the `sql:` evidence and everything it was
  derived from, so an assumption such as a YAML list taken as exhaustive stays
  visible behind any number of `SELECT *` models (D2).
- The result describes the evidence it cites in `evidence`: origin, coverage,
  coverage basis, order, contract enforcement and what it was derived from.
  Only cited records and the records they were derived from are kept, also
  after selecting columns.
- An impact report cites evidence per conclusion. A confirmed or possible
  impact cites what each edge of its path relied on: the evidence of the
  sources or unresolved parts of the dependent that make the edge, what
  identified the slot of the model they read, and what decided that the
  dependent's output exists at its position, such as the width of a star
  before it. A confirmed edge and a possible
  edge to the same output each keep their own evidence. What only another
  input of the dependent relied on is not part of a path. The exploration of
  a kind cites what the search relied on: what identified the input, the shape
  of each examined dependent, and the lineage of each of its outputs as a
  whole. "No impact" relies on the evidence of every requested exploration.
  Row dependencies are not collected, so a row exploration cites nothing. The
  report describes what it cites in `evidence`, as a model result does.
- The plain, Mermaid and DOT outputs name each assumption a result relies on,
  that is a column list taken as exhaustive by the YAML policy or by an
  enforced contract, with the conclusions that rely on it: columns and the
  column list of a model result, and the impacts, explorations or "no impact"
  of an impact report. Outputs with the same name are told apart by their
  slot.

### Cross-model tracing

Tracing a column through models follows the typed lineage of each model's
`ModelAnalysis`, not just a set of source names.

- A `Partial` lineage keeps its `known` sources. They are traced further. Its
  `unresolved` part is carried along instead of being dropped, so the known
  edges do not disappear because something else is unresolved.
- Upstream columns are looked up by output slot, not by sorting names into a
  set. A source column `(relation, column)` is resolved to a dbt node, and the
  column is looked up in that model's slots by the dialect's column key. A
  unique match is followed by slot id. Several matches are never resolved by
  taking the first: the reference and its candidates stay in an
  `AmbiguousOutput` reason that makes the output partial, rather than becoming
  a known terminal source. Duplicate names and unknown positions stay
  distinguishable, and a positional reference does not turn into a name match.
- A model that is referenced by name and cannot be identified (more than one
  candidate) is a boundary, not a guess.
- The order of output slots that one model proves is preserved when another
  model reads it, so a positional set operation over models lines up the same way
  as over inline queries (D1).
- The CTE and split-model forms of the same query give the same result when the
  preconditions in the engine contract hold (I3).

Cases: D1, I3.

### Terminal reasons

Tracing stops in places, and the reason matters more than the fact that it
stopped. A stop point is a `TraceBoundary` with a position (`at`, a model output)
and a reason. `at` is the existing output slot where the reference was written,
not an invented slot of the resource where tracing stopped. Each boundary also
records the identity and kind of the resource
it stands for (model, source, seed, snapshot, or a physical relation) and the
scope or policy that applies to it.

Boundaries are listed on the model result. Each has a `root` output identifying
the column being traced, separate from `at`, so selecting columns also selects
their boundaries while preserving the location of an upstream reference.

| Reason | Meaning |
| --- | --- |
| `ExternalRelation` | An input that is intentionally outside the declared tracing scope. A legitimate end of the dependency chain. It is never a fallback for a dbt node whose required definition is unavailable |
| `SourceFree` | The output has no input column, such as a constant. Not an external source |
| `AnalysisFailed` | The model could not be analysed. The sources are not determined |
| `Cycle` | The traced path loops across models. This is not the cycle of the evaluation context inside one statement, which the engine contract describes |
| `MissingDefinition` | An expected in-scope node or transformation whose required definition is missing |
| `UnsupportedMaterialization` | A definition exists, but its materialization or history semantics are not modelled, as for a snapshot |
| `AmbiguousModel` | A reference matches more than one model. It is never resolved into `ExternalRelation` |

`ExternalRelation` and `SourceFree` are legitimate ends. The other reasons are
places where tracing stopped for a reason other than the data flow ending. A
stop point is never reported as a source that it is not: a constant is not made
into an external source, and a failed model is not called a terminal source
(T1, T2, T4).

**Classification.**

| Target | Classification | Effect on exploration |
| --- | --- | --- |
| Source declared in the manifest | `ExternalRelation` | Legitimate end within the declared scope |
| Seed, when seed contents and loading are outside the tracing scope | `ExternalRelation`, with resource kind `seed` | Legitimate end within that scope |
| Model with compiled SQL, including an ephemeral model | Tracing continues | Depends on the result |
| Model without compiled SQL, including an ephemeral model | `MissingDefinition` | Incomplete. Schema declarations and enforced contracts do not replace the definition, and the model is not treated as a physical table (T5) |
| Snapshot whose required definition is missing | `MissingDefinition` | Incomplete |
| Snapshot with a definition but unmodelled materialization and history semantics | `UnsupportedMaterialization` | Incomplete. The compiled `SELECT` alone does not explain the history (T6) |
| Snapshot under an explicit boundary policy that makes it opaque | `ExternalRelation`, with the policy recorded in the result | Legitimate end within that policy |
| Physical relation not in the manifest, with no expected dbt node identity | `ExternalRelation` | Legitimate end within the declared scope |
| Relation for which the manifest expects a node, but the definition or node is missing | `MissingDefinition` | Incomplete |
| Reference that matches several nodes | `AmbiguousModel` | Incomplete |

A relation in the compiled SQL is matched against the relation each manifest
resource records as the adapter renders it (`relation_name`), which has as
many parts as the adapter qualifies relations with. `database` and `schema`
alone can name a part the adapter does not render, as in ClickHouse. The
relation is built from `database`, `schema` and the identifier only when
`relation_name` is absent, cannot be parsed, or has a last part that is not
the resource's identifier (as for a BigQuery path in one pair of backticks).

An unresolved physical relation is external only when no expected dbt node
identity requires an in-scope definition. `Unsupported` and `MissingDefinition`
boundaries make the relevant exploration incomplete. A snapshot is treated as
external only when the policy is explicit, and the policy is part of the result
and of the cache identity.

Cases: T1, T2, T4, T5, T6.

### Impact

Impact asks which columns are affected by a given input. The result separates
what is confirmed from what is possible, carries a dependency kind on each
impact, and says per kind whether the search was complete.

**Dependency kinds and scope.** A `DependencyKind` is `Value` or `Row`, with the
meanings of the engine contract (section 1). An `ImpactScope` is `ValueOnly` or
`ValueAndRow`. It states which kinds the caller asked for.

```text
ImpactReport {
    scope: ImpactScope,
    confirmed: [ConfirmedImpact],
    possible: [PossibleImpact],
    exploration: [KindExploration],      // one per requested kind
    evidence: [SchemaEvidenceRecord],    // what the report cites
}
ConfirmedImpact { target: ModelOutputRef, kind: DependencyKind, path: [ImpactEdge], evidence: [EvidenceId] }
PossibleImpact  { target: ModelOutputRef, kind: DependencyKind, path: [ImpactEdge], evidence: [EvidenceId] }
ImpactEdge      { from: DependencyRef, to: ModelOutputRef, kind: DependencyKind }
KindExploration { kind: DependencyKind, status: ExplorationStatus, evidence: [EvidenceId] }
ExplorationStatus = Complete | Incomplete { gaps: [TraceGap] }
```

`DependencyRef` names the input of an edge. It is a column today. It leaves room
for the row set of a relation, so that relation-level impact can be added later.

| Field | Meaning |
| --- | --- |
| `confirmed` | Targets that depend on the input through a dependency path established under the adopted evidence, with the kind of that path. It does not mean every data change changes the output |
| `possible` | Concrete targets that might depend on the input through an unresolved path, for example candidates of an ambiguous name |
| `exploration` | For each requested kind, `Complete`, or `Incomplete` with a list of gaps |

**Path composition.** A path of value edges only is a value impact. A path with
any row edge is a row impact. A target can have both kinds through different
paths, and both are kept. A path that depends on a row dependency which is not
resolved is not reported as confirmed.

**Exploration per kind.** `ValueOnly` searches value impact paths. `ValueAndRow`
searches both kinds. Completeness is reported separately for each requested
kind. If row dependencies have not been collected and their relevance cannot be
excluded, the row exploration is `Incomplete` with a `NotCollected` gap. Data
that was not collected is not an empty dependency set, and it does not justify
inventing possible targets. The value exploration can still be `Complete`.

**No impact.** "No impact" is stated only when `confirmed` is empty, `possible`
is empty and the exploration is `Complete` for every requested kind. It holds
only for the reported scope and the adopted evidence, and it lists the evidence
that the conclusion used (see "Assumption visibility"). An empty `confirmed`
alone says nothing, and "no impact" under `ValueOnly` does not mean the input
cannot affect the result rows.

- Known impact edges and unresolved parts coexist. A column with a `Partial`
  lineage is listed as affected through its `known` sources, and its
  unresolved part makes the exploration `Incomplete` unless it can be proven
  to be unrelated to the input (T3).
- When a construct such as an unsupported expression prevents even listing
  candidates, `possible` is not filled with every column. A gap is recorded in
  `exploration` instead.
- The input column is looked up in the slots of its model like any other
  reference. A name that matches no listed slot, matches several, or belongs to
  a model that could not be analysed is an `UnidentifiedInput` gap with a
  diagnostic. The search still continues: the known sources of the dependents
  are matched by relation and by the column key of the name, which their SQL
  states explicitly, and the search goes on by slot from each match.
  The path of such an impact starts at a `DependencyRef::UnidentifiedColumn`.
  It means that the dependent references that column name of the start model;
  it does not establish a dependency on one particular slot of it.
- Paths, visited sets and targets are keyed by model and slot id, so outputs
  with the same name are different targets.
- A dependent model whose output is open is an `OpenOutput` gap unless its
  unexpanded output can be proven unrelated to the input. A relation that could
  not be resolved is not proof of independence: the columns that are not listed
  may depend on the input.
- A dependent output that reads a name shared by several outputs of the input
  model, one of which is the input, is an `AmbiguousInput` gap.
- The existence of the rows of a relation (for example `COUNT(*)`) is not
  expanded into value dependencies on all columns of that relation. A column
  input and the row set of a relation are different inputs.

Cases: T1, T2, T3, T4, D2, D3.

### Column counts

The old `traced_columns` and `total_columns` mix several meanings. They are
replaced by three counts:

| Count | Meaning |
| --- | --- |
| `listed_columns` | Output slots that were enumerated. Duplicate names count separately |
| `value_complete_columns` | Outputs whose value lineage is `Complete`, including constants |
| `value_partial_columns` | Outputs whose value lineage is `Partial` |

Whether the total number of columns is known is read from the shape of the
output, not from these counts. The counts must not be used as a measure of how
completely tracing reached the end, nor of impact exploration.

## Model dependency analysis

This section describes how `dlin-core` reports how completely it analysed the
model-level dependencies (`ref`, `source`, exposure `depends_on`) of a project
in SQL mode. It is the target design; parts of it are not implemented yet.
The vocabulary lives in `src/graph/dependency.rs`.

### Kinds of records

Three kinds of records are kept apart, because they mean different things to
a caller deciding whether a result can be trusted:

| Record | Belongs to | Meaning |
| --- | --- | --- |
| `DependencyReason` | one node | Part of the node's declared dependencies could not be analysed |
| `ProjectGap` | the project | An input was skipped, or a whole scope was not assessed, and the analysis carried on |
| `ConfigDiagnostic` | the project | The configuration may not mean what the user intended |

A reason states that something was not analysed. It does not assert that a
dependency is missing. Reasons are typed: a caller decides which ones to
accept from their `type` and fields, never from message text. The `type` tag
is also the diagnostic code.

A node's `DependencyAnalysis` is `complete` exactly when it has no reasons;
`partial` means at least one reason, not a fraction of the dependencies. The
reasons are authoritative, and the status is checked against them when a
serialised analysis is read back.

An empty analysis claims that the node's dependencies were assessed. A node
that was not assessed carries a `not_assessed` reason; `DependencyAnalysis`
has no default value, so a node cannot become `complete` merely because no
analysis was attached to it.

A reference whose target does not exist still creates a phantom node so the
graph shows it, but the referencing node carries an `unresolved_ref` or
`unresolved_source` reason. An edge to a phantom is not a known dependency.
Every node that references the target carries the reason, not only the first.
An exposure does not create phantoms: a `depends_on` target that does not
exist is a `dependency_target_missing` reason.

Some edges are kept although their target is a guess: a `ref()` naming
another package is resolved by name (`unsupported_ref_package`), and a
`ref()` matching several resources goes to the model, then the seed, then the
snapshot (`ambiguous_ref`). Telling such edges apart from known ones is left
to edge-level certainty, not to the node analysis.

Which nodes are analysed:

| Node | Analysis |
| --- | --- |
| Model, snapshot and singular test SQL | The Jinja extraction's reasons and the reasons raised while resolving its `ref()`/`source()` calls |
| Exposure, YAML snapshot `relation`, YAML generic test arguments | The Jinja written in YAML, selected and interpreted as dbt evaluates it, with the same interpreter as SQL. An exposure entry that yields no dependency and no reason is `unsupported_dependency_expression` |
| YAML generic test | The arguments as above, plus `generic_test_body_not_analyzed`: the body of the `test_<name>` macro is not analysed, so a dependency written only inside it is not known. Built-in names carry the reason too, since a project override or `adapter.dispatch()` can select another implementation |
| Seed, YAML source | Complete: they have no dependencies |
| Phantom | `not_assessed` (`phantom_dependencies`) |
| Semantic model, metric, saved query | `not_assessed` (`semantic_dependencies`) |
| Any node in manifest mode | `not_assessed` (`manifest_dependencies`) |

A reason raised by a call carries the call site in the node that uses it
(`NodeCallSite`). While one file is extracted the owning node is not known
yet, so the extractor records an `ExtractionSite` and the graph builder
attaches the node. `not_assessed` carries only a scope. A phantom node's scope
(`phantom_dependencies`) cannot be turned into a project gap: the uncertainty
of an unresolved target is reported through the reasons of the nodes that
reference it.

### Node and project completeness

The two levels of completeness are kept apart:

- A node is `complete` when its declared dependencies were assessed and no
  reason was recorded for it. This covers only what the node's own analysis
  could see.
- A result about the project, such as an impact exploration, is complete only
  when no node it depends on has a reason that could hide a dependency, and no
  project gap remains: no skipped input and no unassessed scope.

A project gap can affect any node. For example, when a macro file cannot be
read, a model that calls one of its macros may have no reason of its own,
because the call cannot even be recognised as a macro call. Unless the gap is
proved not to affect a result, the result is reported as incomplete.
Configuration diagnostics are not part of either level: whether the settings
are what the user intended is for the user to judge.

### Inputs

dbt treats a search directory that does not exist as empty, whether the path
is the default or written in `dbt_project.yml`. dlin keeps that behaviour and
separates two situations:

- A path that does not exist holds no input to miss. It is a
  `ConfigDiagnostic`, not a gap. At most one warning is raised per project,
  in this order: paths written in `dbt_project.yml` that do not exist
  (`configured_input_path_missing`), default `model-paths` that do not exist
  (`model_paths_missing`), no file found in any searched path
  (`project_inputs_empty`). A default path that does not exist is an info
  diagnostic (`default_input_path_missing`); optional directories are often
  absent.
- An input that exists but could not be inspected, listed, read or parsed,
  while the analysis carried on, is a `ProjectGap` (`input_skipped`, with the
  stage and failure). `project_inputs_empty` is not raised when such a gap
  exists, since the project may not be empty.

Whether a path does not exist or cannot be inspected is decided from its
metadata, not from `exists()`. Directory walks are sorted by file name, so
which of two macros with the same name is used does not depend on the
filesystem. Inputs that are fatal today (a YAML file or a model, snapshot or
test SQL file that cannot be read) stay fatal.

A complete result is relative to the project and the search settings given.
It does not prove that the settings are what the user intended.
