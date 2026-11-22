//! Stderr diagnostics about the model dependency analysis of a project.
//!
//! One normalised set is collected from the graph's node reasons, the
//! project gaps and the configuration diagnostics. The default output (one
//! summary, plus the configuration warning if any) and the verbose output
//! (also one diagnostic per reason and gap) are both generated from it, so
//! the counts in the summary match the verbose diagnostics.
//!
//! The summary describes the whole project. Whether one impact result is
//! complete is stated by that report's `exploration`, not by the summary.
//!
//! The wording of `what`, `why` and `hint` is for people; programs should
//! read `code` and the structured fields.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

use crate::graph::dependency::{
    ConfigDiagnostic, ConfigDiagnosticLevel, DependencyFunction, DependencyReason,
    NodeDependencyReason, ProjectGap, ProjectScope, SettingPath,
};
use crate::graph::types::{LineageGraph, NodeType};

/// The code of the summary diagnostic.
pub const SUMMARY_CODE: &str = "dependency_analysis_summary";

/// The number of namespaces named per reason type in the text summary.
const LISTED_NAMESPACES: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Warning,
    Info,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Warning => "Warning",
            Self::Info => "Info",
        }
    }
}

/// One stderr diagnostic: the common `level`, `code`, `what`, `why` and
/// `hint`, plus fields that depend on the code.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub level: Level,
    pub code: &'static str,
    pub what: String,
    pub why: Option<String>,
    pub hint: Option<String>,
    pub fields: Map<String, Value>,
}

impl Diagnostic {
    /// The diagnostic as one JSON object.
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("level".into(), self.level.as_str().into());
        map.insert("code".into(), self.code.into());
        map.insert("what".into(), self.what.clone().into());
        map.insert("why".into(), self.why.clone().into());
        map.insert("hint".into(), self.hint.clone().into());
        for (key, value) in &self.fields {
            map.insert(key.clone(), value.clone());
        }
        Value::Object(map)
    }

    /// The diagnostic as text, in the layout of other stderr messages.
    pub fn to_text(&self) -> String {
        let mut out = format!("{}: [{}] {}", self.level.label(), self.code, self.what);
        if let Some(why) = &self.why {
            out.push_str(&format!("\n  Why: {why}"));
        }
        if let Some(hint) = &self.hint {
            out.push_str(&format!("\n  Hint: {hint}"));
        }
        out
    }
}

/// The normalised dependency diagnostics of one graph.
#[derive(Debug, Clone)]
pub struct DependencyDiagnostics {
    /// Node reasons with their owning node, phantom nodes left out.
    reasons: Vec<(String, NodeDependencyReason)>,
    project_gaps: Vec<ProjectGap>,
    config: Vec<ConfigDiagnostic>,
    /// The project directory as shown to the user, for configuration
    /// diagnostics whose paths are relative to it.
    project_dir: Option<String>,
}

impl DependencyDiagnostics {
    /// Collect the diagnostics of a graph built before any filtering.
    pub fn collect(
        graph: &LineageGraph,
        project_gaps: &[ProjectGap],
        config: &[ConfigDiagnostic],
        project_dir: Option<String>,
    ) -> Self {
        let mut reasons: Vec<(String, NodeDependencyReason)> = graph
            .node_indices()
            .map(|idx| &graph[idx])
            // A phantom's uncertainty is in the reasons of the nodes that
            // reference it.
            .filter(|node| node.node_type != NodeType::Phantom)
            .flat_map(|node| {
                node.dependency_analysis
                    .unresolved()
                    .iter()
                    .map(|reason| (node.unique_id.clone(), reason.clone()))
            })
            .collect();
        reasons.sort();
        reasons.dedup();
        let mut project_gaps = project_gaps.to_vec();
        project_gaps.sort();
        project_gaps.dedup();
        let mut config = config.to_vec();
        config.sort();
        config.dedup();
        Self {
            reasons,
            project_gaps,
            config,
            project_dir,
        }
    }

    /// The diagnostics shown by default: the configuration warning, if any,
    /// and the summary, if there is anything to summarise.
    pub fn default_diagnostics(&self) -> Vec<Diagnostic> {
        let mut out: Vec<Diagnostic> = self
            .config
            .iter()
            .filter(|diagnostic| diagnostic.level() == ConfigDiagnosticLevel::Warning)
            .map(|diagnostic| self.config_diagnostic(diagnostic))
            .collect();
        out.extend(self.summary());
        out
    }

    /// The diagnostics shown with `--verbose`: every configuration
    /// diagnostic, the summary, and one diagnostic per reason and gap.
    pub fn verbose_diagnostics(&self) -> Vec<Diagnostic> {
        let mut out: Vec<Diagnostic> = self
            .config
            .iter()
            .map(|diagnostic| self.config_diagnostic(diagnostic))
            .collect();
        out.extend(self.summary());
        out.extend(
            self.reasons
                .iter()
                .map(|(unique_id, reason)| reason_diagnostic(unique_id, reason)),
        );
        out.extend(self.project_gaps.iter().map(gap_diagnostic));
        out
    }

    /// The project-wide summary, or `None` when every node was analysed
    /// and there is no project gap.
    pub fn summary(&self) -> Option<Diagnostic> {
        if self.reasons.is_empty() && self.project_gaps.is_empty() {
            return None;
        }
        let mut partial_nodes: BTreeSet<&str> = BTreeSet::new();
        let mut reasoned_nodes: BTreeSet<&str> = BTreeSet::new();
        let mut reason_counts: BTreeMap<&str, usize> = BTreeMap::new();
        let mut namespace_counts: BTreeMap<&str, BTreeMap<&str, usize>> = BTreeMap::new();
        let mut scopes: BTreeSet<ProjectScope> = BTreeSet::new();
        for (unique_id, reason) in &self.reasons {
            reasoned_nodes.insert(unique_id);
            *reason_counts.entry(reason.code()).or_default() += 1;
            if let Some(namespace) = reason.namespace() {
                *namespace_counts
                    .entry(reason.code())
                    .or_default()
                    .entry(namespace)
                    .or_default() += 1;
            }
            match reason {
                DependencyReason::NotAssessed { scope } => scopes.extend(scope.project_scope()),
                _ => {
                    partial_nodes.insert(unique_id);
                }
            }
        }
        let not_assessed_nodes = reasoned_nodes.len() - partial_nodes.len();
        let mut gap_counts: BTreeMap<&str, usize> = BTreeMap::new();
        for gap in &self.project_gaps {
            *gap_counts.entry(gap.code()).or_default() += 1;
            if let ProjectGap::NotAssessed { scope } = gap {
                scopes.insert(*scope);
            }
        }
        let skipped = gap_counts.get("input_skipped").copied().unwrap_or(0);

        let mut parts = Vec::new();
        if !partial_nodes.is_empty() {
            parts.push(format!(
                "{} node(s) partially analysed",
                partial_nodes.len()
            ));
        }
        if not_assessed_nodes > 0 {
            parts.push(format!("{not_assessed_nodes} node(s) not assessed"));
        }
        if skipped > 0 {
            parts.push(format!("{skipped} input(s) skipped"));
        }
        if parts.is_empty() {
            parts.push("some dependencies not assessed".to_string());
        }
        let what = format!(
            "model dependency analysis is incomplete: {}",
            parts.join(", ")
        );

        let mut why_parts = Vec::new();
        let mut by_count: Vec<(&str, usize)> = reason_counts
            .iter()
            .filter(|(code, _)| **code != "not_assessed")
            .map(|(code, count)| (*code, *count))
            .collect();
        by_count.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let reason_text: Vec<String> = by_count
            .iter()
            .map(|(code, count)| match namespace_counts.get(code) {
                Some(namespaces) => {
                    format!("{code} {count} ({})", namespace_list(namespaces))
                }
                None => format!("{code} {count}"),
            })
            .collect();
        if !reason_text.is_empty() {
            why_parts.push(format!("reasons: {}", reason_text.join(", ")));
        }
        if skipped > 0 {
            why_parts.push(format!("skipped inputs: {skipped}"));
        }
        if !scopes.is_empty() {
            why_parts.push(format!(
                "not assessed: {}",
                scopes
                    .iter()
                    .map(|scope| scope_name(*scope))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        let level = if partial_nodes.is_empty() && skipped == 0 {
            Level::Info
        } else {
            Level::Warning
        };
        let mut fields = Map::new();
        fields.insert("partial_nodes".into(), partial_nodes.len().into());
        fields.insert("not_assessed_nodes".into(), not_assessed_nodes.into());
        fields.insert("node_reason_count".into(), self.reasons.len().into());
        fields.insert("project_gap_count".into(), self.project_gaps.len().into());
        fields.insert("reason_counts".into(), json!(reason_counts));
        fields.insert("namespace_counts".into(), json!(namespace_counts));
        fields.insert("project_gap_counts".into(), json!(gap_counts));
        fields.insert(
            "not_assessed_scopes".into(),
            json!(scopes.iter().map(|s| scope_name(*s)).collect::<Vec<_>>()),
        );
        Some(Diagnostic {
            level,
            code: SUMMARY_CODE,
            what,
            why: Some(why_parts.join("; ")),
            hint: Some(
                "run with --verbose to list each reason. These counts cover the whole \
                 project; the `exploration` of an impact report says whether its own \
                 result can miss nodes"
                    .to_string(),
            ),
            fields,
        })
    }

    fn config_diagnostic(&self, diagnostic: &ConfigDiagnostic) -> Diagnostic {
        let level = match diagnostic.level() {
            ConfigDiagnosticLevel::Warning => Level::Warning,
            ConfigDiagnosticLevel::Info => Level::Info,
        };
        let (what, why, hint) = match diagnostic {
            ConfigDiagnostic::ConfiguredInputPathMissing {
                paths,
                all_model_paths_missing,
            } => (
                format!(
                    "paths set in dbt_project.yml do not exist: {}",
                    setting_paths(paths)
                ),
                if *all_model_paths_missing {
                    "no model-paths directory exists, so no model is read; dbt treats a \
                     missing directory as empty"
                } else {
                    "dbt treats a missing directory as empty"
                },
                "check the paths in dbt_project.yml and the --project-dir",
            ),
            ConfigDiagnostic::ModelPathsMissing { paths } => (
                format!(
                    "none of the default model-paths exist: {}",
                    paths.join(", ")
                ),
                "no model is read; dbt treats a missing directory as empty",
                "check the --project-dir, or set model-paths in dbt_project.yml",
            ),
            ConfigDiagnostic::ProjectInputsEmpty {
                project_name,
                searched,
            } => (
                match &self.project_dir {
                    Some(dir) => {
                        format!("no input file was found for project '{project_name}' in {dir}")
                    }
                    None => format!("no input file was found for project '{project_name}'"),
                },
                if searched.is_empty() {
                    "no path was searched"
                } else {
                    "the searched paths hold no SQL, YAML, CSV or macro file"
                },
                "check the --project-dir and the paths in dbt_project.yml",
            ),
            ConfigDiagnostic::DefaultInputPathMissing { setting, path } => (
                format!(
                    "the default {} '{}' does not exist",
                    setting_name(*setting),
                    path
                ),
                "this is normal for optional directories",
                "",
            ),
        };
        let mut fields = Map::new();
        fields.insert(
            "config".into(),
            serde_json::to_value(diagnostic).expect("a config diagnostic serialises"),
        );
        Diagnostic {
            level,
            code: diagnostic.code(),
            what,
            why: Some(why.to_string()),
            hint: (!hint.is_empty()).then(|| hint.to_string()),
            fields,
        }
    }
}

fn namespace_list(namespaces: &BTreeMap<&str, usize>) -> String {
    let mut sorted: Vec<(&str, usize)> = namespaces.iter().map(|(n, c)| (*n, *c)).collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let mut out: Vec<String> = sorted
        .iter()
        .take(LISTED_NAMESPACES)
        .map(|(namespace, count)| format!("{namespace} {count}"))
        .collect();
    if sorted.len() > LISTED_NAMESPACES {
        out.push(format!(
            "{} more namespace(s)",
            sorted.len() - LISTED_NAMESPACES
        ));
    }
    out.join(", ")
}

fn scope_name(scope: ProjectScope) -> &'static str {
    match scope {
        ProjectScope::ManifestDependencies => "manifest_dependencies",
        ProjectScope::SemanticDependencies => "semantic_dependencies",
    }
}

fn setting_name(setting: crate::graph::dependency::PathSetting) -> String {
    serde_json::to_value(setting)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn setting_paths(paths: &[SettingPath]) -> String {
    paths
        .iter()
        .map(|path| format!("{} '{}'", setting_name(path.setting), path.path))
        .collect::<Vec<_>>()
        .join(", ")
}

fn quoted(value: &Option<String>) -> String {
    match value {
        Some(value) => format!("'{value}'"),
        None => "<unknown>".to_string(),
    }
}

/// What a reason says, why it matters, and how to check it.
fn describe(reason: &NodeDependencyReason) -> (String, &'static str, &'static str) {
    const MAY_OMIT: &str = "a dependency of this node may be missing from the graph";
    const MAY_OMIT_OR_ADD: &str =
        "a dependency of this node may be missing from the graph, or an edge may be wrong";
    use DependencyReason as R;
    match reason {
        R::UnresolvedRef {
            name,
            package,
            version,
            ..
        } => {
            let mut call = match package {
                Some(package) => format!("ref('{package}', '{name}'"),
                None => format!("ref('{name}'"),
            };
            if let Some(version) = version {
                call.push_str(&format!(", v={version}"));
            }
            call.push(')');
            (
                format!("{call} does not match any node"),
                MAY_OMIT,
                "check the name, or that the file defining it is in a searched path",
            )
        }
        R::UnresolvedSource { source, table, .. } => (
            format!("source('{source}', '{table}') is not declared"),
            MAY_OMIT,
            "check the source declaration in the YAML files",
        ),
        R::UnresolvedVar { name, .. } => (
            format!("var({}) has no value and no default", quoted(name)),
            MAY_OMIT_OR_ADD,
            "set the variable in dbt_project.yml or give the call a default",
        ),
        R::UnsupportedDispatch {
            name, namespace, ..
        } => (
            match namespace {
                Some(namespace) => format!(
                    "adapter.dispatch({}, macro_namespace='{namespace}') is not resolved",
                    quoted(name)
                ),
                None => format!("adapter.dispatch({}) is not resolved", quoted(name)),
            },
            MAY_OMIT_OR_ADD,
            "check the dispatched implementations by hand",
        ),
        R::UnsupportedMacroNamespace {
            namespace,
            macro_name,
            ..
        } => (
            format!(
                "{namespace}.{macro_name}() is called through a namespace, which is not analysed"
            ),
            MAY_OMIT_OR_ADD,
            "check whether the macro reads ref() or source()",
        ),
        R::UnresolvedCall { callee, .. } => (
            format!("the call {} could not be identified", quoted(callee)),
            MAY_OMIT_OR_ADD,
            "check that the macro is defined in a searched macro path",
        ),
        R::UnsupportedJinjaConstruct { construct, .. } => (
            format!("the Jinja construct '{construct}' is not modelled"),
            MAY_OMIT_OR_ADD,
            "check the dependencies of this part of the template by hand",
        ),
        R::InvalidDependencyCall { function, .. } => (
            format!(
                "{}() is called with arguments that are not valid",
                match function {
                    DependencyFunction::Ref => "ref",
                    DependencyFunction::Source => "source",
                }
            ),
            MAY_OMIT,
            "check the arguments of the call",
        ),
        R::AnalysisLimitReached { limit, .. } => (
            format!(
                "the analysis stopped at its {} limit",
                serde_json::to_value(limit)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default()
            ),
            MAY_OMIT_OR_ADD,
            "check for deeply recursive macros",
        ),
        R::RuntimeValueUnknown { source, name, .. } => (
            format!(
                "a dependency depends on {} {}, known only when dbt runs",
                serde_json::to_value(source)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
                quoted(name)
            ),
            MAY_OMIT_OR_ADD,
            "check the dependency with a manifest generated for the target you run",
        ),
        R::JinjaRenderFailed { .. } => (
            "the Jinja template could not be interpreted".to_string(),
            MAY_OMIT_OR_ADD,
            "check the template syntax",
        ),
        R::MacroReachabilityUnknown { macro_name, .. } => (
            format!(
                "the macros reachable from {} could not be determined",
                quoted(macro_name)
            ),
            MAY_OMIT_OR_ADD,
            "check which macro the call selects",
        ),
        R::UnsupportedDependencyExpression { function, .. } => (
            format!(
                "the depends_on entry {} is not a supported expression",
                quoted(function)
            ),
            MAY_OMIT,
            "write the entry as ref('name') or source('source', 'table')",
        ),
        R::DependencyTargetMissing { name, .. } => (
            format!("the depends_on target '{name}' does not exist"),
            MAY_OMIT,
            "check the name of the target",
        ),
        R::UnsupportedRefPackage { package, name, .. } => (
            format!("ref('{package}', '{name}') names a package; it is resolved by name only"),
            MAY_OMIT_OR_ADD,
            "check that the edge goes to the intended node",
        ),
        R::AmbiguousRef {
            name, candidates, ..
        } => (
            format!(
                "ref('{name}') matches several nodes: {}",
                candidates.join(", ")
            ),
            MAY_OMIT_OR_ADD,
            "rename one of the nodes so that the name is unique",
        ),
        R::GenericTestBodyNotAnalyzed { test_name, .. } => (
            format!(
                "the body of the generic test {} is not analysed",
                quoted(test_name)
            ),
            MAY_OMIT,
            "check whether the test macro reads ref() or source() besides its arguments",
        ),
        R::NotAssessed { scope } => (
            format!(
                "the dependencies are not assessed ({})",
                serde_json::to_value(scope)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default()
            ),
            "the dependencies of this node are taken as they are, without analysis",
            "",
        ),
    }
}

fn reason_diagnostic(unique_id: &str, reason: &NodeDependencyReason) -> Diagnostic {
    let (description, why, hint) = describe(reason);
    let what = match reason.at() {
        Some(at) => {
            let mut location = at.path.clone();
            if let Some(line) = at.line {
                location.push_str(&format!(":{line}"));
            }
            match &at.expr {
                Some(expr) => format!("{unique_id} ({location}, `{expr}`): {description}"),
                None => format!("{unique_id} ({location}): {description}"),
            }
        }
        None => format!("{unique_id}: {description}"),
    };
    let level = match reason {
        DependencyReason::NotAssessed { .. } => Level::Info,
        _ => Level::Warning,
    };
    let mut fields = Map::new();
    fields.insert("unique_id".into(), unique_id.into());
    fields.insert(
        "reason".into(),
        serde_json::to_value(reason).expect("a reason serialises"),
    );
    Diagnostic {
        level,
        code: reason.code(),
        what,
        why: Some(why.to_string()),
        hint: (!hint.is_empty()).then(|| hint.to_string()),
        fields,
    }
}

fn gap_diagnostic(gap: &ProjectGap) -> Diagnostic {
    let (level, what, why, hint) = match gap {
        ProjectGap::InputSkipped {
            location,
            kind,
            stage,
            failure,
        } => {
            let name = |value: Value| value.as_str().unwrap_or_default().replace('_', " ");
            (
                Level::Warning,
                format!(
                    "{}: {} input skipped at the {} step ({})",
                    location.path,
                    name(json!(kind)),
                    name(json!(stage)),
                    name(json!(failure)),
                ),
                "the analysis carried on without it, so any node may miss a dependency",
                Some("fix or remove the input".to_string()),
            )
        }
        ProjectGap::NotAssessed { scope } => (
            Level::Info,
            format!(
                "the {} are not assessed",
                scope_name(*scope).replace('_', " ")
            ),
            "these dependencies are taken as they are, without analysis",
            None,
        ),
    };
    let mut fields = Map::new();
    fields.insert("unique_id".into(), Value::Null);
    fields.insert(
        "gap".into(),
        serde_json::to_value(gap).expect("a project gap serialises"),
    );
    Diagnostic {
        level,
        code: gap.code(),
        what,
        why: Some(why.to_string()),
        hint,
        fields,
    }
}

#[cfg(test)]
mod tests;
