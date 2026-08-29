use super::*;
use std::fs;

mod compatibility {
    use super::*;
    include!("tests/compatibility.rs");
}
mod identifiers {
    use super::*;
    include!("tests/identifiers.rs");
}
mod graph {
    use super::*;
    include!("tests/graph.rs");
}
mod contents {
    use super::*;
    include!("tests/contents.rs");
}
mod semantic_layer {
    use super::*;
    include!("tests/semantic_layer.rs");
}
