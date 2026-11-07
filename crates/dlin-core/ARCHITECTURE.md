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
and so on) that tests refer to are listed in section 8 of the engine contract.

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

Cases: E1, E2, E3, E4.

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

### Evidence and cache keys

Schema evidence is part of what an analysis result depends on. The cache key
therefore covers the model SQL, the dialect, and the evidence that was adopted
for the model and its upstream models (by evidence fingerprint). When any of it
changes, for example when YAML evidence is replaced by catalog evidence (E4),
the cached entry is invalid and a hit is not allowed.

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
- The CTE and split-model forms of the same query give the same result when the
  preconditions in the engine contract hold (I3).

### Terminal reasons

Tracing stops in places, and the reason matters more than the fact that it
stopped. A stop point is a `TraceBoundary` with a position (`at`, a model output)
and a reason:

| Reason | Meaning |
| --- | --- |
| `ExternalRelation` | A declared source or a relation with no model definition. A legitimate end of the dependency chain |
| `SourceFree` | The output has no input column, such as a constant. Not an external source |
| `AnalysisFailed` | The model could not be analysed. The sources are not determined |
| `Cycle` | The traced path loops |
| `MissingDefinition` | A referenced model has no definition |
| `AmbiguousModel` | A reference matches more than one model |

`ExternalRelation` and `SourceFree` are legitimate ends. The other reasons are
places where tracing stopped for a reason other than the data flow ending. A
stop point is never reported as a source that it is not: a constant is not made
into an external source, and a failed model is not called a terminal source
(T1, T2, T4).

### Impact

Impact asks which columns are affected by a given input column. The result
separates what is confirmed from what is possible, and says whether the search
was complete:

| Field | Meaning |
| --- | --- |
| `scope` | What was searched: the value dependencies only, or also row dependencies |
| `confirmed` | Columns that depend on the input by a known value dependency |
| `possible` | Concrete columns that might depend on it, for example candidates of an ambiguous name |
| `exploration` | `Complete`, or `Incomplete` with a list of gaps |

"No impact" is stated only when `confirmed` is empty, `possible` is empty and
`exploration` is `Complete`. An empty `confirmed` alone says nothing.

- Known impact edges and unresolved parts coexist. A column with a `Partial`
  lineage is listed as affected through its `known` sources, and its
  unresolved part makes the exploration `Incomplete` unless it can be proven
  to be unrelated to the input (T3).
- When a construct such as an unsupported expression prevents even listing
  candidates, `possible` is not filled with every column. A gap is recorded in
  `exploration` instead.
- Completeness is relative to the scope. An impact search over value
  dependencies is not "complete for all dependencies". Until row dependencies are
  collected, a search that includes them is not reported as complete.

Cases: T1, T2, T3, T4.

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
