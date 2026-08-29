use clap::Parser;

mod cli;
#[cfg(not(tarpaulin_include))]
mod commands;
#[cfg(not(tarpaulin_include))]
mod mcp;

use cli::{Cli, ColumnCommand, Command, ErrorFormat};
use dlin_core::input;

/// Reset SIGPIPE to default behavior so broken pipes terminate the process
/// silently instead of causing panics. Rust's runtime sets SIG_IGN on SIGPIPE,
/// which turns pipe closures into EPIPE errors that panic via `.unwrap()`.
#[cfg(unix)]
fn reset_sigpipe() {
    // Safety: signal() is a standard POSIX function. Restoring SIG_DFL for
    // SIGPIPE is safe and matches the expected behavior of CLI tools.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}
#[cfg(not(tarpaulin_include))]
fn main() {
    #[cfg(unix)]
    reset_sigpipe();

    let cli = Cli::parse();

    // Set error format before anything else so warnings/errors use it
    dlin_core::set_error_format_json(cli.error_format == ErrorFormat::Json);

    let result = match cli.command {
        Command::Graph(args) => {
            dlin_core::set_quiet(args.quiet);
            commands::run_graph_command(args)
        }
        Command::List(args) => {
            dlin_core::set_quiet(args.quiet);
            commands::run_list_command(args)
        }
        Command::Summary(args) => {
            dlin_core::set_quiet(args.quiet);
            commands::run_summary_command(args)
        }
        Command::CheckManifest(args) => {
            dlin_core::set_quiet(args.quiet);
            commands::run_check_manifest_command(args)
        }
        Command::Column(col) => match col.command {
            ColumnCommand::Upstream(args) => {
                dlin_core::set_quiet(args.quiet);
                commands::run_column_lineage_command(
                    args.model,
                    &args.column,
                    &args.output,
                    args.dialect,
                    &args.project_dir,
                    args.manifest_path.as_ref(),
                    args.cache_dir.as_deref(),
                    args.no_cache,
                    args.refresh_cache,
                )
            }
            ColumnCommand::Downstream(args) => {
                dlin_core::set_quiet(args.quiet);
                commands::run_column_impact_command(
                    &args.model,
                    &args.column,
                    &args.output,
                    args.dialect,
                    &args.project_dir,
                    args.manifest_path.as_ref(),
                    args.cache_dir.as_deref(),
                    args.no_cache,
                    args.refresh_cache,
                )
            }
        },
        Command::Debug(args) => commands::run_debug_command(args),
        Command::Mcp(args) => {
            dlin_core::set_quiet(true);
            mcp::run(args)
        }
        Command::Impact {
            model,
            project_dir,
            output,
            source,
            manifest_path,
            cache_dir,
            no_cache,
            refresh_cache,
            quiet,
        } => {
            dlin_core::set_quiet(quiet);
            let stdin_lines = input::read_stdin_lines();
            let mut raw_inputs = model;
            raw_inputs.extend(stdin_lines);
            if raw_inputs.is_empty() {
                Err(anyhow::anyhow!(
                    "no model names provided (specify as arguments or via stdin)"
                ))
            } else {
                commands::run_impact_command(
                    raw_inputs,
                    &project_dir,
                    &output,
                    &source,
                    manifest_path.as_ref(),
                    cache_dir.as_deref(),
                    no_cache,
                    refresh_cache,
                )
            }
        }
    };

    if let Err(err) = result {
        let diag = dlin_core::Diagnostic::from_error(&err);
        eprintln!("{}", dlin_core::format_diagnostic(&diag));
        std::process::exit(1);
    }
}

// Keep a valid binary target when coverage instrumentation excludes the CLI
// entry point and its command implementations.
#[cfg(tarpaulin_include)]
fn main() {}
