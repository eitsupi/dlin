//! Compare the interpreter with the placeholder render on a project.
//!
//! Development tool for switching extractors: every difference in the ref
//! and source calls of a SQL file is matched with how the interpreter
//! treated the call, so that only differences without such a basis need a
//! closer look.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use super::{DecisionOutcome, DependencyExtraction, MacroLibrary, extract_dependencies};
use crate::graph::dependency::DependencyFunction;
use crate::parser::discovery::discover_files;
use crate::parser::jinja::reachability::PreparedMacroPrefix;
use crate::parser::project::DbtProject;
use crate::parser::sql::{RefCall, SourceCall, extract_all_with_prepared_prefix};

/// Why a call differs between the two extractors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DifferenceKind {
    /// Only the render has it; the name is computed (it is not a literal
    /// call anywhere) and the interpreter rejected a computed call of the
    /// same function in the file because an argument is unknown.
    RejectedUnknownArgument,
    /// Only the render has it; the call is written only in macros that the
    /// interpreter did not call.
    MacroNotCalled,
    /// Only the render has it; the interpreter found the call in a branch
    /// whose condition is known to be false.
    KnownFalseBranch,
    /// Only the interpreter has it, under an unknown condition.
    UnknownCondition,
    /// Only the interpreter has it, from known values (a loop over a known
    /// list, a macro the render could not run).
    ResolvedByInterpreter,
    /// No basis was found. Needs a closer look.
    Unexplained,
}

#[derive(Debug, Clone, Serialize)]
pub struct Difference {
    pub file: String,
    pub kind: DifferenceKind,
    /// `render` or `interpreter`: which extractor has the call.
    pub only_in: &'static str,
    pub call: String,
}

#[derive(Debug, Default, Serialize)]
pub struct ComparisonReport {
    pub files: usize,
    pub identical_files: usize,
    pub counts: BTreeMap<String, usize>,
    /// Number of files whose interpreter result has at least one reason.
    pub partial_files: usize,
    /// Number of reasons by type, over all files.
    pub reason_counts: BTreeMap<String, usize>,
    pub config_differences: Vec<String>,
    pub differences: Vec<Difference>,
}

fn describe_ref(call: &RefCall) -> String {
    let mut text = match &call.package {
        Some(package) => format!("ref('{package}', '{}'", call.name),
        None => format!("ref('{}'", call.name),
    };
    if let Some(version) = &call.version {
        text.push_str(&format!(", v={version}"));
    }
    text.push(')');
    text
}

fn describe_source(call: &SourceCall) -> String {
    format!("source('{}', '{}')", call.source_name, call.table_name)
}

/// Where literal ref/source calls are written: in the template body or in a
/// named macro.
#[derive(Default)]
struct LiteralIndex {
    /// Calls written outside any macro.
    body: (Vec<RefCall>, Vec<SourceCall>),
    /// Whether the body has a call with a computed argument.
    body_dynamic: bool,
    /// Calls written in each macro, by macro name, and whether the macro
    /// has a call with a computed argument.
    macros: Vec<(std::sync::Arc<str>, Vec<RefCall>, Vec<SourceCall>, bool)>,
}

impl LiteralIndex {
    fn add_macro(&mut self, def: &super::ir::MacroDef) {
        let mut found = super::eval::StaticCalls::default();
        found.scan_stmts(&def.body);
        self.macros
            .push((def.name.clone(), found.refs, found.sources, found.dynamic));
        for stmt in &def.body {
            self.add_nested(stmt);
        }
    }

    fn add_nested(&mut self, stmt: &super::ir::Stmt) {
        if let super::ir::Stmt::Macro(def) = stmt {
            self.add_macro(def);
        }
    }

    fn for_template(sql: &str, library: &MacroLibrary) -> Self {
        let mut index = LiteralIndex::default();
        for def in library.iter() {
            index.add_macro(def);
        }
        if let Ok(template) = super::convert::parse_template(sql, super::ir::SourceId::Template) {
            let mut found = super::eval::StaticCalls::default();
            let body: Vec<_> = template
                .body
                .iter()
                .filter(|stmt| !matches!(stmt, super::ir::Stmt::Macro(_)))
                .cloned()
                .collect();
            found.scan_stmts(&body);
            index.body = (found.refs, found.sources);
            index.body_dynamic = found.dynamic;
            for stmt in &template.body {
                index.add_nested(stmt);
            }
        }
        index
    }

    /// Whether the call is written as a literal anywhere, and whether every
    /// such place is a macro that was not called.
    fn locate(
        &self,
        matches: impl Fn(&[RefCall], &[SourceCall]) -> bool,
        entered: &std::collections::BTreeSet<std::sync::Arc<str>>,
    ) -> (bool, bool) {
        let in_body = matches(&self.body.0, &self.body.1);
        let mut found = in_body;
        let mut only_uncalled = !in_body;
        for (name, refs, sources, _) in &self.macros {
            if matches(refs, sources) {
                found = true;
                if entered.contains(name) {
                    only_uncalled = false;
                }
            }
        }
        (found, found && only_uncalled)
    }
}

impl LiteralIndex {
    /// Whether every call with a computed argument is in a macro that was
    /// not called.
    fn computed_calls_only_uncalled(
        &self,
        entered: &std::collections::BTreeSet<std::sync::Arc<str>>,
    ) -> bool {
        let mut any = false;
        if self.body_dynamic {
            return false;
        }
        for (name, _, _, dynamic) in &self.macros {
            if *dynamic {
                any = true;
                if entered.contains(name) {
                    return false;
                }
            }
        }
        any
    }
}

fn classify_missing(
    new: &DependencyExtraction,
    index: &LiteralIndex,
    function: DependencyFunction,
    names: &[&str],
    literal: impl Fn(&[RefCall], &[SourceCall]) -> bool,
    same: impl Fn(&super::Decision) -> bool,
) -> DifferenceKind {
    let decisions = || new.decisions.iter().filter(|d| d.function == function);
    if decisions().any(|d| d.outcome == DecisionOutcome::SkippedKnownFalse && same(d)) {
        return DifferenceKind::KnownFalseBranch;
    }
    let (written, only_uncalled) = index.locate(literal, &new.entered);
    if only_uncalled {
        return DifferenceKind::MacroNotCalled;
    }
    let placeholder = names
        .iter()
        .any(|name| name.contains("__dbt_") || name.is_empty());
    let rejected = decisions().any(|d| d.outcome == DecisionOutcome::Rejected);
    if placeholder || (!written && rejected) {
        return DifferenceKind::RejectedUnknownArgument;
    }
    if !written && index.computed_calls_only_uncalled(&new.entered) {
        return DifferenceKind::MacroNotCalled;
    }
    DifferenceKind::Unexplained
}

fn classify_added(
    new: &DependencyExtraction,
    same: impl Fn(&super::Decision) -> bool,
) -> DifferenceKind {
    let accepted = new.decisions.iter().filter(|d| same(d));
    let mut kind = DifferenceKind::Unexplained;
    for decision in accepted {
        match decision.outcome {
            DecisionOutcome::Accepted { conditional: true } => {
                return DifferenceKind::UnknownCondition;
            }
            DecisionOutcome::Accepted { conditional: false } => {
                kind = DifferenceKind::ResolvedByInterpreter
            }
            _ => {}
        }
    }
    kind
}

/// Compare both extractors on every SQL file of a project.
pub fn compare_project(project_dir: &Path) -> Result<ComparisonReport> {
    let project = DbtProject::load(project_dir)?;
    let files = discover_files(&project.resolve_paths(project_dir))?;
    let macro_sources: Vec<(std::path::PathBuf, String)> = files
        .macro_sql_files
        .iter()
        .filter_map(|path| Some((path.clone(), std::fs::read_to_string(path).ok()?)))
        .collect();
    let sources: Vec<String> = macro_sources.iter().map(|(_, s)| s.clone()).collect();
    let prefix_source = crate::parser::jinja::build_macro_prefix(&sources);
    let prefix = PreparedMacroPrefix::new(&prefix_source);
    let library = MacroLibrary::new(&macro_sources);
    let vars: &HashMap<String, serde_json::Value> = &project.vars;

    let mut report = ComparisonReport::default();
    let sql_files = files
        .model_sql_files
        .iter()
        .chain(&files.snapshot_sql_files)
        .chain(&files.test_sql_files);
    for path in sql_files {
        let Ok(sql) = std::fs::read_to_string(path) else {
            continue;
        };
        let base = crate::input::normalize_path(project_dir);
        let file = crate::graph::dependency::relative_display_path(path, &base);
        report.files += 1;
        let old = extract_all_with_prepared_prefix(&sql, &prefix, vars);
        let new = extract_dependencies(&sql, &library, vars);
        let index = LiteralIndex::for_template(&sql, &library);
        if !new.reasons.is_empty() {
            report.partial_files += 1;
        }
        for reason in &new.reasons {
            *report
                .reason_counts
                .entry(reason.code().to_string())
                .or_default() += 1;
        }
        let before = report.differences.len();
        for call in old.refs.iter().filter(|c| !new.refs.contains(c)) {
            let kind = classify_missing(
                &new,
                &index,
                DependencyFunction::Ref,
                &[call.name.as_str()],
                |refs, _| {
                    refs.iter()
                        .any(|r| r.name == call.name && r.package == call.package)
                },
                |d| {
                    d.reference
                        .as_ref()
                        .is_some_and(|r| r.name == call.name && r.package == call.package)
                },
            );
            report.differences.push(Difference {
                file: file.clone(),
                kind,
                only_in: "render",
                call: describe_ref(call),
            });
        }
        for call in new.refs.iter().filter(|c| !old.refs.contains(c)) {
            let kind = classify_added(&new, |d| d.reference.as_ref() == Some(call));
            report.differences.push(Difference {
                file: file.clone(),
                kind,
                only_in: "interpreter",
                call: describe_ref(call),
            });
        }
        for call in old.sources.iter().filter(|c| !new.sources.contains(c)) {
            let kind = classify_missing(
                &new,
                &index,
                DependencyFunction::Source,
                &[call.source_name.as_str(), call.table_name.as_str()],
                |_, sources| sources.contains(call),
                |d| d.source.as_ref() == Some(call),
            );
            report.differences.push(Difference {
                file: file.clone(),
                kind,
                only_in: "render",
                call: describe_source(call),
            });
        }
        for call in new.sources.iter().filter(|c| !old.sources.contains(c)) {
            let kind = classify_added(&new, |d| d.source.as_ref() == Some(call));
            report.differences.push(Difference {
                file: file.clone(),
                kind,
                only_in: "interpreter",
                call: describe_source(call),
            });
        }
        if old.config.materialized != new.config.materialized || old.config.tags != new.config.tags
        {
            report.config_differences.push(format!(
                "{file}: render {:?} {:?}, interpreter {:?} {:?}",
                old.config.materialized, old.config.tags, new.config.materialized, new.config.tags
            ));
        }
        if report.differences.len() == before {
            report.identical_files += 1;
        }
    }
    for difference in &report.differences {
        let key = serde_json::to_value(difference.kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        *report.counts.entry(key).or_default() += 1;
    }
    Ok(report)
}
