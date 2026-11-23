/// The known limitations of column lineage, shared by the help of `dlin column`
/// and its subcommands. A macro, so that `concat!` can join it to the rest of
/// each help text.
macro_rules! column_limitations {
    () => {
        "\
Limitations:
  - Value dependencies only: columns used only in filters and join
    conditions (row dependencies) are not collected.
  - Unknown columns behind SELECT *, unsupported SQL and ambiguous names
    leave lineage partial.
  - A STRUCT field can be traced to the inputs of the whole STRUCT, so a
    complete result can include the inputs of sibling fields. This happens
    across dbt model boundaries, and through CAST, CASE, set operations,
    array subscripts or UNNEST of an array built elsewhere.
  - A YAML column list used as a schema is treated as exhaustive. Results
    that rely on it cite it in evidence[] and show an \"assumes\" note."
    };
}

mod column;
mod command;
mod common;
mod graph;
mod misc;
mod summary;

pub use column::*;
pub use command::*;
pub use common::*;
pub use graph::*;
pub use misc::*;
pub use summary::*;
