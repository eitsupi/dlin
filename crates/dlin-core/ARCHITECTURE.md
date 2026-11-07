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

### Evidence and cache keys

Schema evidence is part of what an analysis result depends on. Each cache layer
is keyed by the inputs of that layer:

| Cache | Identity includes |
| --- | --- |
| SQL analysis | The SQL, the dialect and normalization settings, the adopted evidence (by evidence fingerprint), the analysis policy (for example the YAML coverage policy), and an engine and contract compatibility identity |
| Node resolution and tracing | Everything above, plus the inputs that resolve relations to dbt nodes and the boundary policy |
| A cache that combines layers | The union of the inputs of the layers it combines |

When any of it changes, for example when YAML evidence is replaced by catalog
evidence (E4, E5), the cached entry is invalid and a hit is not allowed. A change
only in the relation-to-node inputs invalidates the node-resolution and tracing
results without forcing the pure SQL analysis to run again.

The existing package-version compatibility checks remain. They are not enough
alone: a development build whose analysis semantics changed under the same
package version must not reuse old results. The engine and contract
compatibility identity covers this.

Stored results keep their evidence references, so loading an entry gives back
the same information that was stored (I4).

### Cross-model tracing

Tracing a column through models follows the typed lineage of each model's
`ModelAnalysis`, not just a set of source names.

- A `Partial` lineage keeps its `known` sources. They are traced further. Its
  `unresolved` part is carried along instead of being dropped, so the known
  edges do not disappear because something else is unresolved.
- Upstream columns are looked up by output slot, not by sorting names into a
  set. Duplicate names and unknown positions stay distinguishable, and a
  positional reference does not turn into a name match.
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
and a reason. Each boundary also records the identity and kind of the resource
it stands for (model, source, seed, snapshot, or a physical relation) and the
scope or policy that applies to it.

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
