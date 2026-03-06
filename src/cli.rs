use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "dlin", about = "A fast CLI tool for dbt model lineage analysis")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Visualize dbt model lineage graph
    Graph {
        /// Model name to focus on (shows full lineage if omitted)
        model: Option<String>,

        /// Path to dbt project directory
        #[arg(short = 'p', long = "project-dir", default_value = ".")]
        project_dir: PathBuf,

        /// Upstream levels to show (default: all)
        #[arg(short = 'u', long)]
        upstream: Option<usize>,

        /// Downstream levels to show (default: all)
        #[arg(short = 'd', long)]
        downstream: Option<usize>,

        /// Launch interactive TUI mode
        #[arg(short = 'i', long)]
        interactive: bool,

        /// Output format: ascii (default), dot, json, mermaid, svg, html
        #[arg(short = 'o', long, default_value = "ascii")]
        output: OutputFormat,

        /// Include test nodes
        #[arg(long)]
        include_tests: bool,

        /// Include seed nodes
        #[arg(long)]
        include_seeds: bool,

        /// Include snapshot nodes
        #[arg(long)]
        include_snapshots: bool,

        /// Include exposure nodes
        #[arg(long)]
        include_exposures: bool,

        /// Selector expression: tag:X, path:Y, or model name (comma-separated)
        #[arg(short = 's', long)]
        select: Option<String>,

        /// Use manifest.json instead of parsing SQL (path to manifest file or directory containing target/manifest.json)
        #[arg(long)]
        manifest: Option<PathBuf>,
    },

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

        /// Use manifest.json instead of parsing SQL
        #[arg(long)]
        manifest: Option<PathBuf>,
    },

    /// Compare lineage between git refs
    Diff {
        /// Base git ref to compare from (e.g., main, HEAD~1)
        #[arg(long)]
        base: String,

        /// Head git ref to compare to (defaults to working tree)
        #[arg(long)]
        head: Option<String>,

        /// Path to dbt project directory
        #[arg(short = 'p', long = "project-dir", default_value = ".")]
        project_dir: PathBuf,

        /// Output format: text (default) or json
        #[arg(short = 'o', long, default_value = "text")]
        output: DiffOutputFormat,
    },
}

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum OutputFormat {
    Ascii,
    Dot,
    Json,
    Mermaid,
    Svg,
    Html,
}

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum ImpactOutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum DiffOutputFormat {
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
    fn test_graph_default_args() {
        let cli = Cli::try_parse_from(["dlin", "graph"]).unwrap();
        match cli.command {
            Command::Graph {
                ref model,
                interactive,
                ref upstream,
                ref downstream,
                include_tests,
                include_seeds,
                include_snapshots,
                include_exposures,
                ref select,
                ref manifest,
                ref output,
                ..
            } => {
                assert!(model.is_none());
                assert!(!interactive);
                assert!(upstream.is_none());
                assert!(downstream.is_none());
                assert!(!include_tests);
                assert!(!include_seeds);
                assert!(!include_snapshots);
                assert!(!include_exposures);
                assert!(select.is_none());
                assert!(manifest.is_none());
                assert!(matches!(output, OutputFormat::Ascii));
            }
            _ => panic!("Expected Graph subcommand"),
        }
    }

    #[test]
    fn test_graph_all_flags() {
        let cli = Cli::try_parse_from([
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
        .unwrap();
        match cli.command {
            Command::Graph {
                ref model,
                ref project_dir,
                ref upstream,
                ref downstream,
                interactive,
                ref output,
                include_tests,
                include_seeds,
                include_snapshots,
                include_exposures,
                ref select,
                ..
            } => {
                assert_eq!(model.as_deref(), Some("my_model"));
                assert_eq!(project_dir, &PathBuf::from("/path/to/project"));
                assert_eq!(*upstream, Some(2));
                assert_eq!(*downstream, Some(3));
                assert!(interactive);
                assert!(matches!(output, OutputFormat::Dot));
                assert!(include_tests);
                assert!(include_seeds);
                assert!(include_snapshots);
                assert!(include_exposures);
                assert_eq!(select.as_deref(), Some("tag:nightly,path:models/staging"));
            }
            _ => panic!("Expected Graph subcommand"),
        }
    }

    #[test]
    fn test_graph_select_short_flag() {
        let cli = Cli::try_parse_from(["dlin", "graph", "-s", "orders,tag:nightly"]).unwrap();
        match cli.command {
            Command::Graph { ref select, .. } => {
                assert_eq!(select.as_deref(), Some("orders,tag:nightly"));
            }
            _ => panic!("Expected Graph subcommand"),
        }
    }

    #[test]
    fn test_graph_manifest_flag() {
        let cli =
            Cli::try_parse_from(["dlin", "graph", "--manifest", "/path/to/manifest.json"])
                .unwrap();
        match cli.command {
            Command::Graph { ref manifest, .. } => {
                assert_eq!(manifest, &Some(PathBuf::from("/path/to/manifest.json")));
            }
            _ => panic!("Expected Graph subcommand"),
        }
    }

    #[test]
    fn test_graph_output_formats() {
        for (fmt, expected) in [
            ("ascii", "Ascii"),
            ("dot", "Dot"),
            ("json", "Json"),
            ("mermaid", "Mermaid"),
            ("svg", "Svg"),
            ("html", "Html"),
        ] {
            let cli = Cli::try_parse_from(["dlin", "graph", "-o", fmt]).unwrap();
            match cli.command {
                Command::Graph { ref output, .. } => {
                    assert_eq!(format!("{:?}", output), expected);
                }
                _ => panic!("Expected Graph subcommand"),
            }
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
    fn test_diff_subcommand() {
        let cli = Cli::try_parse_from(["dlin", "diff", "--base", "main"]).unwrap();
        match cli.command {
            Command::Diff {
                ref base, ref head, ..
            } => {
                assert_eq!(base, "main");
                assert!(head.is_none());
            }
            _ => panic!("Expected Diff subcommand"),
        }
    }

    #[test]
    fn test_diff_subcommand_with_head() {
        let cli =
            Cli::try_parse_from(["dlin", "diff", "--base", "main", "--head", "feature"])
                .unwrap();
        match cli.command {
            Command::Diff {
                ref base, ref head, ..
            } => {
                assert_eq!(base, "main");
                assert_eq!(head.as_deref(), Some("feature"));
            }
            _ => panic!("Expected Diff subcommand"),
        }
    }
}
