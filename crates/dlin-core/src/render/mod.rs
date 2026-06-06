use std::io;

pub mod ascii;
#[cfg(feature = "column-lineage")]
pub mod column_graph;
pub mod dot;
pub mod html;
pub mod impact;
pub mod json;
pub mod layout;
pub mod list;
pub mod mermaid;
pub mod plain;
pub mod summary;
pub mod svg;

/// Handle an I/O result from writing to stdout.
/// Silently ignores `BrokenPipe` errors (e.g. `cmd | head`).
pub(crate) fn handle_stdout_result(result: io::Result<()>) {
    if let Err(e) = result
        && e.kind() != io::ErrorKind::BrokenPipe
    {
        eprintln!("error writing output: {}", e);
    }
}

/// Capitalize the first letter of a string (e.g. "model" -> "Model")
pub(crate) fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}

/// Label used for nodes that have no file_path when grouping by directory.
pub(crate) const NO_DIRECTORY_LABEL: &str = "(other)";

/// Extract the directory portion from a node's file_path for directory grouping.
/// Returns the parent directory as a string (e.g. "models/staging"), or
/// `NO_DIRECTORY_LABEL` if the node has no file_path.
pub(crate) fn directory_label(node: &crate::graph::types::NodeData) -> String {
    match &node.file_path {
        Some(path) => path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| {
                // Normalize separators to '/' for consistent output across platforms
                p.components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<String>>()
                    .join("/")
            })
            .unwrap_or_else(|| NO_DIRECTORY_LABEL.to_string()),
        None => NO_DIRECTORY_LABEL.to_string(),
    }
}

/// Escape characters that are special inside Mermaid double-quoted labels.
///
/// Mermaid uses `#entity;` syntax (not HTML `&entity;`).
/// We escape `"`, `<`, `>`, and `#` so user-provided text cannot break
/// the label syntax or interfere with `<br/>` separators we insert.
pub(crate) fn mermaid_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '#' => out.push_str("#num;"),
            '"' => out.push_str("#quot;"),
            '<' => out.push_str("#lt;"),
            '>' => out.push_str("#gt;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Escape characters that are special inside DOT double-quoted strings.
///
/// DOT requires `\` and `"` to be backslash-escaped inside double-quoted
/// strings. Without this, user-provided text (e.g. YAML `label:` values)
/// containing these characters would produce syntactically invalid DOT output.
pub(crate) fn dot_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(ch),
        }
    }
    out
}

/// Sanitize a string into a valid identifier for DOT/Mermaid (only `[A-Za-z0-9_]`).
pub(crate) fn sanitize_id(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Convert a `serde_json::Error` into an `io::Error`, preserving the
/// underlying I/O error kind (e.g. `BrokenPipe`) when present.
pub(crate) fn serde_io_error(e: serde_json::Error) -> io::Error {
    match e.io_error_kind() {
        Some(kind) => io::Error::new(kind, e),
        None => io::Error::other(e),
    }
}

#[cfg(test)]
pub(crate) mod test_helpers {
    use crate::graph::types::*;

    pub fn make_node(unique_id: &str, label: &str, node_type: NodeType) -> NodeData {
        NodeData {
            unique_id: unique_id.into(),
            label: label.into(),
            node_type,
            file_path: None,
            description: None,
            materialization: None,
            tags: vec![],
            columns: vec![],
            exposure: None,
            aliases: vec![],
        }
    }

    pub fn make_node_with_columns(
        unique_id: &str,
        label: &str,
        node_type: NodeType,
        columns: &[&str],
    ) -> NodeData {
        NodeData {
            unique_id: unique_id.into(),
            label: label.into(),
            node_type,
            file_path: None,
            description: None,
            materialization: None,
            tags: vec![],
            columns: columns.iter().map(|s| s.to_string()).collect(),
            exposure: None,
            aliases: vec![],
        }
    }

    pub fn make_node_with_path(
        unique_id: &str,
        label: &str,
        node_type: NodeType,
        path: &str,
    ) -> NodeData {
        NodeData {
            unique_id: unique_id.into(),
            label: label.into(),
            node_type,
            file_path: Some(path.into()),
            description: None,
            materialization: None,
            tags: vec![],
            columns: vec![],
            exposure: None,
            aliases: vec![],
        }
    }

    /// Build a representative lineage graph for snapshot tests:
    /// source -> staging -> mart -> test, mart -> exposure
    pub fn make_sample_lineage_graph() -> LineageGraph {
        let mut graph = LineageGraph::new();
        let src = graph.add_node(make_node(
            "source.raw.orders",
            "raw.orders",
            NodeType::Source,
        ));
        let stg = graph.add_node(make_node("model.stg_orders", "stg_orders", NodeType::Model));
        let mart = graph.add_node(make_node("model.orders", "orders", NodeType::Model));
        let t = graph.add_node(make_node(
            "test.orders_positive",
            "orders_positive",
            NodeType::Test,
        ));
        let exp = graph.add_node(make_node(
            "exposure.dashboard",
            "dashboard",
            NodeType::Exposure,
        ));

        graph.add_edge(src, stg, EdgeData::direct(EdgeType::Source));
        graph.add_edge(stg, mart, EdgeData::direct(EdgeType::Ref));
        graph.add_edge(mart, t, EdgeData::direct(EdgeType::Test));
        graph.add_edge(mart, exp, EdgeData::direct(EdgeType::Exposure));

        graph
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dot_escape_plain() {
        assert_eq!(dot_escape("hello"), "hello");
    }

    #[test]
    fn test_dot_escape_double_quote() {
        assert_eq!(dot_escape(r#"a "b" c"#), r#"a \"b\" c"#);
    }

    #[test]
    fn test_dot_escape_backslash() {
        assert_eq!(dot_escape(r"a\b"), r"a\\b");
    }

    #[test]
    fn test_dot_escape_both() {
        assert_eq!(dot_escape(r#"a\"b"#), r#"a\\\"b"#);
    }

    #[test]
    fn test_sanitize_id_directory_with_hyphens() {
        assert_eq!(sanitize_id("models/my-project"), "models_my_project");
    }

    #[test]
    fn test_sanitize_id_parentheses() {
        assert_eq!(sanitize_id("(other)"), "_other_");
    }

    #[test]
    fn test_sanitize_id_empty() {
        assert_eq!(sanitize_id(""), "");
    }

    #[test]
    fn test_sanitize_id_already_valid() {
        assert_eq!(sanitize_id("models_staging"), "models_staging");
    }

    #[test]
    fn test_sanitize_id_non_ascii() {
        assert_eq!(sanitize_id("モデル/日本語"), "_______");
    }

    #[test]
    fn test_capitalize() {
        assert_eq!(capitalize("model"), "Model");
        assert_eq!(capitalize(""), "");
    }

    #[test]
    fn test_directory_label_with_path() {
        let node = test_helpers::make_node_with_path(
            "model.a",
            "a",
            crate::graph::types::NodeType::Model,
            "models/staging/a.sql",
        );
        assert_eq!(directory_label(&node), "models/staging");
    }

    #[test]
    fn test_directory_label_without_path() {
        let node =
            test_helpers::make_node("exposure.e", "e", crate::graph::types::NodeType::Exposure);
        assert_eq!(directory_label(&node), NO_DIRECTORY_LABEL);
    }

    #[test]
    fn test_directory_label_file_at_root() {
        let node = test_helpers::make_node_with_path(
            "model.a",
            "a",
            crate::graph::types::NodeType::Model,
            "a.sql",
        );
        assert_eq!(directory_label(&node), NO_DIRECTORY_LABEL);
    }
}
