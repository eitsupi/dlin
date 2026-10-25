# dlin-sqllineage

SQL table and column lineage engine used by [dlin](https://github.com/eitsupi/dlin).

This crate is a fork of [funcpp/sqllineage](https://github.com/funcpp/sqllineage),
maintained inside the dlin repository. Its API differs from upstream and it is
primarily intended for dlin, so semver stability for external users is not a goal.

## Structured analysis

The public API provides ordered output columns, typed unresolved sources, and
borrowed catalogs.

```rust
use dlin_sqllineage::{Analyzer, Dialect, Lineage};

let statement = Analyzer::new(Dialect::Generic)
    .analyze("SELECT id FROM events")?
    .into_single()?;
assert_eq!(statement.output.columns[0].ordinal, Some(0));
assert!(matches!(statement.output.columns[0].lineage, Lineage::Complete(_)));
# Ok::<(), Box<dyn std::error::Error>>(())
```

Syntax errors fail the input. Semantic errors produce an error diagnostic and
empty output columns for the affected statement while preserving other results.
`Shape` describes the final output; unresolved stars inside nested queries are
represented in column lineage when they contribute to that output.
`StatementAnalysis::has_unresolved_stars` records schema uncertainty anywhere in
the statement. Each `UnexpandedStar` also retains known value dependencies in
its `lineage`, such as the source of a struct value being expanded.

`parse` returns a reusable `ParsedSql` with `ast_debug`, `ast_json`, and `analyze`
methods. JSON representations of ASTs and analysis results are debug output and
are not stable contracts. Statement order, output order, and deterministic
source ordering are part of the API.

## Origin

- Upstream repository: https://github.com/funcpp/sqllineage
- Base revision: `b010d8083c60aee977055ea36f8a6807c1d51c62`
- The code keeps upstream's MIT OR Apache-2.0 license.

## License

Licensed under either of

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option.
