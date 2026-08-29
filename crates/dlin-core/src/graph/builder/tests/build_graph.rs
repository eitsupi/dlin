use super::*;
use std::collections::HashMap;

mod basic {
    use super::*;
    include!("build_graph/basic.rs");
}
mod yaml_snapshots {
    use super::*;
    include!("build_graph/yaml_snapshots.rs");
}
mod semantic_layer {
    use super::*;
    include!("build_graph/semantic_layer.rs");
}
