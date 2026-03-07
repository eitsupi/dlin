use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "dlin", about = "A fast CLI tool for dbt model lineage analysis", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, clap::Args)]
pub struct GraphArgs {
    /// Model name to focus on (shows full lineage if omitted)
    pub model: Option<String>,

    /// Path to dbt project directory
    #[arg(short = 'p', long = "project-dir", default_value = ".")]
    pub project_dir: PathBuf,

    /// Upstream levels to show (default: all)
    #[arg(short = 'u', long)]
    pub upstream: Option<usize>,

    /// Downstream levels to show (default: all)
    #[arg(short = 'd', long)]
    pub downstream: Option<usize>,

    /// Launch interactive TUI mode
    #[arg(short = 'i', long)]
    pub interactive: bool,

    /// Output format: ascii (default), dot, json, mermaid, plain, svg, html
    #[arg(short = 'o', long, default_value = "ascii")]
    pub output: OutputFormat,

    /// Include test nodes
    #[arg(long)]
    pub include_tests: bool,

    /// Include seed nodes
    #[arg(long)]
    pub include_seeds: bool,

    /// Include snapshot nodes
    #[arg(long)]
    pub include_snapshots: bool,

    /// Include exposure nodes
    #[arg(long)]
    pub include_exposures: bool,

    /// Selector expression: tag:X, path:Y, or model name (comma-separated)
    #[arg(short = 's', long)]
    pub select: Option<String>,

    /// Filter output by node type (comma-separated: model,source,seed,snapshot,test,exposure)
    #[arg(long = "node-type", value_delimiter = ',')]
    pub node_types: Option<Vec<String>>,

    /// Data source: sql (parse SQL files, default) or manifest (use manifest.json)
    #[arg(long, default_value = "sql")]
    pub source: SourceType,

    /// Path to manifest.json file or directory containing target/manifest.json (required when --source manifest)
    #[arg(long)]
    pub manifest_path: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Visualize dbt model lineage graph
    Graph(GraphArgs),

    /// Compute downstream impact analysis for a model
    Impact {
        /// Model name to analyze impact for
        model: String,

        /// Path to dbt project directory
        #[arg(short = 'p', long = "project-dir", default_value = ".")]
        project_dir: PathBuf,

        /// Output format: text (default) or json
        #[arg(short = 'o', long, default_value = "text")]
        output: ImpactOutputFormat,

        /// Data source: sql (parse SQL files, default) or manifest (use manifest.json)
        #[arg(long, default_value = "sql")]
        source: SourceType,

        /// Path to manifest.json file or directory containing target/manifest.json (required when --source manifest)
        #[arg(long)]
        manifest_path: Option<PathBuf>,
    },

}

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum OutputFormat {
    Ascii,
    Dot,
    Json,
    Mermaid,
    Plain,
    Svg,
    Html,
}

#[derive(Debug, Clone, PartialEq, Eq, clap::ValueEnum)]
pub enum SourceType {
    /// Parse SQL files directly (default)
    Sql,
    /// Use dbt manifest.json
    Manifest,
}

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum ImpactOutputFormat {
    Text,
    Json,
}


#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_no_subcommand_shows_help() {
        // With no subcommand, clap should error (which triggers help display)
        let result = Cli::try_parse_from(["dlin"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_version_flag() {
        let result = Cli::try_parse_from(["dlin", "--version"]);
        // clap exits with an error (DisplayVersion) when --version is passed
        let err = result.unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayVersion);
    }

    fn unwrap_graph(cli: Cli) -> GraphArgs {
        match cli.command {
            Command::Graph(args) => args,
            _ => panic!("Expected Graph subcommand"),
        }
    }

    #[test]
    fn test_graph_default_args() {
        let args = unwrap_graph(Cli::try_parse_from(["dlin", "graph"]).unwrap());
        assert!(args.model.is_none());
        assert!(!args.interactive);
        assert!(args.upstream.is_none());
        assert!(args.downstream.is_none());
        assert!(!args.include_tests);
        assert!(!args.include_seeds);
        assert!(!args.include_snapshots);
        assert!(!args.include_exposures);
        assert!(args.select.is_none());
        assert_eq!(args.source, SourceType::Sql);
        assert!(args.manifest_path.is_none());
        assert!(matches!(args.output, OutputFormat::Ascii));
    }

    #[test]
    fn test_graph_all_flags() {
        let args = unwrap_graph(
            Cli::try_parse_from([
                "dlin",
                "graph",
                "my_model",
                "-p",
                "/path/to/project",
                "-u",
                "2",
                "-d",
                "3",
                "-i",
                "-o",
                "dot",
                "--include-tests",
                "--include-seeds",
                "--include-snapshots",
                "--include-exposures",
                "--select",
                "tag:nightly,path:models/staging",
            ])
            .unwrap(),
        );
        assert_eq!(args.model.as_deref(), Some("my_model"));
        assert_eq!(args.project_dir, PathBuf::from("/path/to/project"));
        assert_eq!(args.upstream, Some(2));
        assert_eq!(args.downstream, Some(3));
        assert!(args.interactive);
        assert!(matches!(args.output, OutputFormat::Dot));
        assert!(args.include_tests);
        assert!(args.include_seeds);
        assert!(args.include_snapshots);
        assert!(args.include_exposures);
        assert_eq!(
            args.select.as_deref(),
            Some("tag:nightly,path:models/staging")
        );
    }

    #[test]
    fn test_graph_select_short_flag() {
        let args =
            unwrap_graph(Cli::try_parse_from(["dlin", "graph", "-s", "orders,tag:nightly"]).unwrap());
        assert_eq!(args.select.as_deref(), Some("orders,tag:nightly"));
    }

    #[test]
    fn test_graph_select_long_flag() {
        let args = unwrap_graph(
            Cli::try_parse_from(["dlin", "graph", "--select", "path:models/staging"]).unwrap(),
        );
        assert_eq!(args.select.as_deref(), Some("path:models/staging"));
    }

    #[test]
    fn test_graph_source_default_is_sql() {
        let args = unwrap_graph(Cli::try_parse_from(["dlin", "graph"]).unwrap());
        assert_eq!(args.source, SourceType::Sql);
        assert!(args.manifest_path.is_none());
    }

    #[test]
    fn test_graph_source_manifest_with_path() {
        let args = unwrap_graph(
            Cli::try_parse_from([
                "dlin",
                "graph",
                "--source",
                "manifest",
                "--manifest-path",
                "/path/to/manifest.json",
            ])
            .unwrap(),
        );
        assert_eq!(args.source, SourceType::Manifest);
        assert_eq!(
            args.manifest_path,
            Some(PathBuf::from("/path/to/manifest.json"))
        );
    }

    #[test]
    fn test_graph_source_manifest_directory() {
        let args = unwrap_graph(
            Cli::try_parse_from([
                "dlin",
                "graph",
                "--source",
                "manifest",
                "--manifest-path",
                "/path/to/project",
            ])
            .unwrap(),
        );
        assert_eq!(args.source, SourceType::Manifest);
        assert_eq!(
            args.manifest_path,
            Some(PathBuf::from("/path/to/project"))
        );
    }

    #[test]
    fn test_graph_output_formats() {
        for (fmt, expected) in [
            ("ascii", "Ascii"),
            ("dot", "Dot"),
            ("json", "Json"),
            ("mermaid", "Mermaid"),
            ("plain", "Plain"),
            ("svg", "Svg"),
            ("html", "Html"),
        ] {
            let args =
                unwrap_graph(Cli::try_parse_from(["dlin", "graph", "-o", fmt]).unwrap());
            assert_eq!(format!("{:?}", args.output), expected);
        }

        // Invalid format
        let result = Cli::try_parse_from(["dlin", "graph", "-o", "yaml"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_impact_subcommand() {
        let cli =
            Cli::try_parse_from(["dlin", "impact", "orders", "-p", "/path/to/project"])
                .unwrap();
        match cli.command {
            Command::Impact {
                ref model,
                ref project_dir,
                ..
            } => {
                assert_eq!(model, "orders");
                assert_eq!(project_dir, &PathBuf::from("/path/to/project"));
            }
            _ => panic!("Expected Impact subcommand"),
        }
    }

    #[test]
    fn test_impact_subcommand_json() {
        let cli = Cli::try_parse_from(["dlin", "impact", "orders", "-o", "json"]).unwrap();
        match cli.command {
            Command::Impact { ref output, .. } => {
                assert!(matches!(output, ImpactOutputFormat::Json));
            }
            _ => panic!("Expected Impact subcommand"),
        }
    }

    #[test]
    fn test_graph_node_type_single() {
        let args = unwrap_graph(
            Cli::try_parse_from(["dlin", "graph", "--node-type", "model"]).unwrap(),
        );
        assert_eq!(args.node_types, Some(vec!["model".to_string()]));
    }

    #[test]
    fn test_graph_node_type_multiple() {
        let args = unwrap_graph(
            Cli::try_parse_from(["dlin", "graph", "--node-type", "model,source"]).unwrap(),
        );
        assert_eq!(
            args.node_types,
            Some(vec!["model".to_string(), "source".to_string()])
        );
    }

    #[test]
    fn test_graph_node_type_default_none() {
        let args = unwrap_graph(Cli::try_parse_from(["dlin", "graph"]).unwrap());
        assert!(args.node_types.is_none());
    }
}
