//! Vocabulary for reporting how completely the model-level dependencies of a
//! project were analysed.
//!
//! A reason states that part of a node's declared dependencies could not be
//! analysed. It does not assert that a dependency is missing: the unanalysed
//! part may or may not create dependencies. Reasons are typed so that callers
//! can decide which ones they accept by inspecting their fields, never by
//! parsing human-readable messages.
//!
//! Three kinds of records are kept apart:
//!
//! - [`DependencyReason`] belongs to one node. Reasons raised by a call are
//!   located at the call site inside the node that uses it.
//! - [`ProjectGap`] is a gap that cannot be attributed to a node, such as an
//!   input that was skipped while the analysis carried on.
//! - [`ConfigDiagnostic`] reports a project configuration that may not mean
//!   what the user intended, such as a configured path that does not exist.
//!   It is not a gap: a path that does not exist holds no input to miss.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where a reason was raised, as known while extracting one SQL file.
///
/// The owning node is not known yet at that point; the graph builder turns
/// this into a [`NodeCallSite`] once it is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExtractionSite {
    /// 1-based line in the file, when known.
    pub line: Option<u32>,
    /// The call expression as written in the file, when known.
    pub expr: Option<String>,
}

/// The call site in the node that uses a construct, for reasons raised by a
/// call. For a cause inside a macro this is the call in the using node, not
/// the macro definition.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeCallSite {
    pub unique_id: String,
    /// Path relative to the project directory, with `/` separators.
    pub path: String,
    /// 1-based line in `path`, when known.
    pub line: Option<u32>,
    /// The call expression as written in `path`, when known.
    pub expr: Option<String>,
}

impl ExtractionSite {
    /// Attach the owning node and its project-relative path.
    pub fn at_node(self, unique_id: impl Into<String>, path: impl Into<String>) -> NodeCallSite {
        NodeCallSite {
            unique_id: unique_id.into(),
            path: path.into(),
            line: self.line,
            expr: self.expr,
        }
    }
}

/// Which dependency function a call was meant to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyFunction {
    Ref,
    Source,
}

/// Where a value that is unknown during static analysis comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeValueSource {
    /// An attribute of `target`, such as `target.name`.
    TargetAttribute,
    /// `env_var()`. Its value is unknown even when a default is given,
    /// because the analysis does not read the environment.
    EnvVar,
    /// The result of `run_query()`.
    RunQuery,
    /// The result of a `statement` block or `load_result()`.
    Statement,
}

/// A bound the analysis stopped at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisLimit {
    RecursionDepth,
    WorkBudget,
}

/// A scope whose dependencies are not assessed at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotAssessedScope {
    /// Dependencies read from a manifest are not checked.
    ManifestDependencies,
    /// Dependencies of semantic YAML nodes (semantic models, metrics, saved
    /// queries) are not checked in SQL mode.
    SemanticDependencies,
    /// A phantom node stands for an unresolved target; it has no definition
    /// to analyse. Its uncertainty is reported through the reasons of the
    /// nodes that reference it, so this scope never becomes a project gap.
    PhantomDependencies,
}

impl NotAssessedScope {
    /// The project-level scope to report, if this scope is reported at all.
    pub fn project_scope(self) -> Option<ProjectScope> {
        match self {
            Self::ManifestDependencies => Some(ProjectScope::ManifestDependencies),
            Self::SemanticDependencies => Some(ProjectScope::SemanticDependencies),
            Self::PhantomDependencies => None,
        }
    }
}

/// A not-assessed scope that is reported as a project gap. Phantom
/// dependencies are deliberately not representable here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectScope {
    ManifestDependencies,
    SemanticDependencies,
}

/// Why part of a node's declared dependencies could not be analysed.
///
/// `L` is the location type: [`ExtractionSite`] while extracting one file,
/// [`NodeCallSite`] once the owning node is known (see
/// [`NodeDependencyReason`]).
///
/// A dynamic target whose name cannot be determined has `None` in the name
/// field; internal placeholder strings are never reported.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DependencyReason<L> {
    /// A `ref()` whose target does not exist in the project.
    UnresolvedRef {
        name: String,
        package: Option<String>,
        version: Option<String>,
        at: L,
    },
    /// A `source()` whose target is not declared in the project.
    UnresolvedSource {
        source: String,
        table: String,
        at: L,
    },
    /// A `var()` without a value or default.
    UnresolvedVar { name: Option<String>, at: L },
    /// An `adapter.dispatch()` call, which the analysis does not resolve.
    UnsupportedDispatch {
        name: Option<String>,
        namespace: Option<String>,
        at: L,
    },
    /// A call to a macro through a namespace, such as `dbt_utils.star()`.
    /// The namespace is what the call names; no package ownership is
    /// implied.
    UnsupportedMacroNamespace {
        namespace: String,
        #[serde(rename = "macro")]
        macro_name: String,
        at: L,
    },
    /// A call whose callee could not be identified, either a method whose
    /// receiver is unknown or a direct call to an unknown function.
    UnresolvedCall { callee: Option<String>, at: L },
    /// A Jinja construct, operator or filter whose meaning the analysis does
    /// not model.
    UnsupportedJinjaConstruct { construct: String, at: L },
    /// A `ref()` or `source()` call whose arguments are malformed.
    InvalidDependencyCall { function: DependencyFunction, at: L },
    /// The analysis stopped at a bound. Raised even when a root cause is
    /// known, because the unexplored part may hold other causes.
    AnalysisLimitReached { limit: AnalysisLimit, at: L },
    /// A value only known when dbt runs selected or built a dependency.
    RuntimeValueUnknown {
        source: RuntimeValueSource,
        /// What was read, such as `target.name` or an environment variable
        /// name. Never the value.
        name: Option<String>,
        at: L,
    },
    /// Rendering the Jinja template failed.
    JinjaRenderFailed { at: L },
    /// The macros a call can reach could not be determined.
    MacroReachabilityUnknown {
        #[serde(rename = "macro")]
        macro_name: Option<String>,
        at: L,
    },
    /// An exposure `depends_on` entry that is not a supported expression.
    UnsupportedDependencyExpression { function: Option<String>, at: L },
    /// An exposure `depends_on` entry whose target does not exist.
    DependencyTargetMissing { name: String, at: L },
    /// A `ref()` naming a package, which the analysis does not resolve.
    UnsupportedRefPackage {
        package: String,
        name: String,
        at: L,
    },
    /// A `ref()` matching several nodes.
    AmbiguousRef {
        name: String,
        candidates: Vec<String>,
        at: L,
    },
    /// The node's dependencies are not assessed at all.
    NotAssessed { scope: NotAssessedScope },
}

/// A reason attached to a node, located at the node's call site.
pub type NodeDependencyReason = DependencyReason<NodeCallSite>;

/// A reason recorded while extracting one file.
pub type ExtractedDependencyReason = DependencyReason<ExtractionSite>;

impl<L> DependencyReason<L> {
    /// The `type` tag, which is also the diagnostic code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnresolvedRef { .. } => "unresolved_ref",
            Self::UnresolvedSource { .. } => "unresolved_source",
            Self::UnresolvedVar { .. } => "unresolved_var",
            Self::UnsupportedDispatch { .. } => "unsupported_dispatch",
            Self::UnsupportedMacroNamespace { .. } => "unsupported_macro_namespace",
            Self::UnresolvedCall { .. } => "unresolved_call",
            Self::UnsupportedJinjaConstruct { .. } => "unsupported_jinja_construct",
            Self::InvalidDependencyCall { .. } => "invalid_dependency_call",
            Self::AnalysisLimitReached { .. } => "analysis_limit_reached",
            Self::RuntimeValueUnknown { .. } => "runtime_value_unknown",
            Self::JinjaRenderFailed { .. } => "jinja_render_failed",
            Self::MacroReachabilityUnknown { .. } => "macro_reachability_unknown",
            Self::UnsupportedDependencyExpression { .. } => "unsupported_dependency_expression",
            Self::DependencyTargetMissing { .. } => "dependency_target_missing",
            Self::UnsupportedRefPackage { .. } => "unsupported_ref_package",
            Self::AmbiguousRef { .. } => "ambiguous_ref",
            Self::NotAssessed { .. } => "not_assessed",
        }
    }

    /// The location of the reason, if it has one.
    pub fn at(&self) -> Option<&L> {
        match self {
            Self::UnresolvedRef { at, .. }
            | Self::UnresolvedSource { at, .. }
            | Self::UnresolvedVar { at, .. }
            | Self::UnsupportedDispatch { at, .. }
            | Self::UnsupportedMacroNamespace { at, .. }
            | Self::UnresolvedCall { at, .. }
            | Self::UnsupportedJinjaConstruct { at, .. }
            | Self::InvalidDependencyCall { at, .. }
            | Self::AnalysisLimitReached { at, .. }
            | Self::RuntimeValueUnknown { at, .. }
            | Self::JinjaRenderFailed { at }
            | Self::MacroReachabilityUnknown { at, .. }
            | Self::UnsupportedDependencyExpression { at, .. }
            | Self::DependencyTargetMissing { at, .. }
            | Self::UnsupportedRefPackage { at, .. }
            | Self::AmbiguousRef { at, .. } => Some(at),
            Self::NotAssessed { .. } => None,
        }
    }

    /// Convert the location, keeping every other field.
    pub fn map_site<M>(self, f: impl FnOnce(L) -> M) -> DependencyReason<M> {
        use DependencyReason as R;
        match self {
            R::UnresolvedRef {
                name,
                package,
                version,
                at,
            } => R::UnresolvedRef {
                name,
                package,
                version,
                at: f(at),
            },
            R::UnresolvedSource { source, table, at } => R::UnresolvedSource {
                source,
                table,
                at: f(at),
            },
            R::UnresolvedVar { name, at } => R::UnresolvedVar { name, at: f(at) },
            R::UnsupportedDispatch {
                name,
                namespace,
                at,
            } => R::UnsupportedDispatch {
                name,
                namespace,
                at: f(at),
            },
            R::UnsupportedMacroNamespace {
                namespace,
                macro_name,
                at,
            } => R::UnsupportedMacroNamespace {
                namespace,
                macro_name,
                at: f(at),
            },
            R::UnresolvedCall { callee, at } => R::UnresolvedCall { callee, at: f(at) },
            R::UnsupportedJinjaConstruct { construct, at } => R::UnsupportedJinjaConstruct {
                construct,
                at: f(at),
            },
            R::InvalidDependencyCall { function, at } => R::InvalidDependencyCall {
                function,
                at: f(at),
            },
            R::AnalysisLimitReached { limit, at } => R::AnalysisLimitReached { limit, at: f(at) },
            R::RuntimeValueUnknown { source, name, at } => R::RuntimeValueUnknown {
                source,
                name,
                at: f(at),
            },
            R::JinjaRenderFailed { at } => R::JinjaRenderFailed { at: f(at) },
            R::MacroReachabilityUnknown { macro_name, at } => R::MacroReachabilityUnknown {
                macro_name,
                at: f(at),
            },
            R::UnsupportedDependencyExpression { function, at } => {
                R::UnsupportedDependencyExpression {
                    function,
                    at: f(at),
                }
            }
            R::DependencyTargetMissing { name, at } => {
                R::DependencyTargetMissing { name, at: f(at) }
            }
            R::UnsupportedRefPackage { package, name, at } => R::UnsupportedRefPackage {
                package,
                name,
                at: f(at),
            },
            R::AmbiguousRef {
                name,
                candidates,
                at,
            } => R::AmbiguousRef {
                name,
                candidates,
                at: f(at),
            },
            R::NotAssessed { scope } => R::NotAssessed { scope },
        }
    }
}

/// Whether every declared dependency of a node was analysed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyAnalysisStatus {
    /// No reason was recorded for the node.
    Complete,
    /// At least one reason was recorded. This is not a fraction of the
    /// dependencies that were analysed.
    Partial,
}

/// The dependency analysis result of one node.
///
/// The reasons are authoritative: the status is `complete` exactly when
/// there are none. The reasons are deduplicated and sorted, so equal
/// analyses serialise identically.
///
/// An empty analysis claims that the node's declared dependencies were
/// assessed. A node whose dependencies were not assessed must carry a
/// [`DependencyReason::NotAssessed`] reason instead. This type deliberately
/// has no `Default`, so that a node cannot become `complete` merely because
/// no analysis was attached to it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "DependencyAnalysisRepr", try_from = "DependencyAnalysisRepr")]
pub struct DependencyAnalysis {
    unresolved: Vec<NodeDependencyReason>,
}

impl DependencyAnalysis {
    /// The result of an analysis that was performed, with the reasons it
    /// recorded.
    pub fn new(unresolved: impl IntoIterator<Item = NodeDependencyReason>) -> Self {
        let mut unresolved: Vec<_> = unresolved.into_iter().collect();
        unresolved.sort();
        unresolved.dedup();
        Self { unresolved }
    }

    pub fn status(&self) -> DependencyAnalysisStatus {
        if self.unresolved.is_empty() {
            DependencyAnalysisStatus::Complete
        } else {
            DependencyAnalysisStatus::Partial
        }
    }

    pub fn unresolved(&self) -> &[NodeDependencyReason] {
        &self.unresolved
    }

    /// Add reasons, keeping the reasons deduplicated and sorted.
    pub fn extend(&mut self, reasons: impl IntoIterator<Item = NodeDependencyReason>) {
        self.unresolved.extend(reasons);
        self.unresolved.sort();
        self.unresolved.dedup();
    }
}

#[derive(Serialize, Deserialize)]
struct DependencyAnalysisRepr {
    status: DependencyAnalysisStatus,
    unresolved: Vec<NodeDependencyReason>,
}

impl From<DependencyAnalysis> for DependencyAnalysisRepr {
    fn from(analysis: DependencyAnalysis) -> Self {
        Self {
            status: analysis.status(),
            unresolved: analysis.unresolved,
        }
    }
}

impl TryFrom<DependencyAnalysisRepr> for DependencyAnalysis {
    type Error = String;

    fn try_from(repr: DependencyAnalysisRepr) -> Result<Self, Self::Error> {
        let analysis = DependencyAnalysis::new(repr.unresolved);
        if analysis.status() != repr.status {
            return Err(format!(
                "dependency_analysis status {:?} does not match its unresolved reasons",
                repr.status
            ));
        }
        Ok(analysis)
    }
}

/// A location of an input that does not belong to a node.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct InputLocation {
    /// Path relative to the project directory, with `/` separators. Paths
    /// outside the project directory start with `..`.
    pub path: String,
    /// 1-based line in `path`, when known.
    pub line: Option<u32>,
}

/// What kind of input was skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    /// A resource file found while walking a directory.
    File,
    /// A configured directory, or a directory tree being walked.
    Directory,
    /// A YAML properties file.
    Yaml,
    /// A macro file.
    Macro,
}

/// Which step failed for a skipped input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputStage {
    /// Inspecting a configured path.
    Metadata,
    /// Listing the entries of a directory tree.
    Enumerate,
    /// Reading a file.
    Read,
    /// Parsing a file.
    Parse,
    /// Combining a macro file with the other macro files.
    MacroPrefix,
}

/// Why an input was skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputFailure {
    PermissionDenied,
    /// The input disappeared after it was found.
    NotFound,
    /// A configured path exists but is not a directory.
    NotADirectory,
    InvalidUtf8,
    /// The content could not be parsed as the expected format (Jinja or
    /// YAML syntax, or YAML that does not match the expected structure).
    ParseError,
    /// Any other I/O failure.
    IoError,
}

impl InputFailure {
    /// Classify an I/O error. UTF-8 decoding failures are not I/O errors;
    /// callers that decode text report [`InputFailure::InvalidUtf8`]
    /// themselves.
    pub fn from_io_error(error: &std::io::Error) -> Self {
        match error.kind() {
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            std::io::ErrorKind::NotFound => Self::NotFound,
            std::io::ErrorKind::NotADirectory => Self::NotADirectory,
            _ => Self::IoError,
        }
    }
}

/// A gap that cannot be attributed to a node.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProjectGap {
    /// An input was skipped and the analysis carried on without it.
    InputSkipped {
        location: InputLocation,
        kind: InputKind,
        stage: InputStage,
        failure: InputFailure,
    },
    /// A whole scope was not assessed.
    NotAssessed { scope: ProjectScope },
}

impl ProjectGap {
    /// The `type` tag, which is also the diagnostic code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InputSkipped { .. } => "input_skipped",
            Self::NotAssessed { .. } => "not_assessed",
        }
    }
}

/// A `dbt_project.yml` setting naming directories to search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PathSetting {
    #[serde(rename = "model-paths")]
    ModelPaths,
    #[serde(rename = "seed-paths")]
    SeedPaths,
    #[serde(rename = "snapshot-paths")]
    SnapshotPaths,
    #[serde(rename = "test-paths")]
    TestPaths,
    #[serde(rename = "macro-paths")]
    MacroPaths,
    #[serde(rename = "analysis-paths")]
    AnalysisPaths,
}

/// Whether a path setting was written in `dbt_project.yml` or filled in
/// with dbt's default.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum PathOrigin {
    #[default]
    Default,
    Explicit,
}

/// A configured path, relative to the project directory.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SettingPath {
    pub setting: PathSetting,
    pub path: String,
}

/// Severity of a [`ConfigDiagnostic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigDiagnosticLevel {
    Warning,
    Info,
}

/// A project configuration that may not mean what the user intended.
///
/// These are not gaps. A path that does not exist holds no input to miss;
/// whether the configuration is what the user meant is outside what the
/// dependency analysis can prove. At most one warning is produced per
/// project, in the order the variants are listed; the info diagnostics are
/// shown only on request.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConfigDiagnostic {
    /// Paths written in `dbt_project.yml` do not exist.
    ConfiguredInputPathMissing {
        paths: Vec<SettingPath>,
        /// Whether none of the `model-paths` entries exist (explicit or
        /// default).
        all_model_paths_missing: bool,
    },
    /// None of the default `model-paths` entries exist.
    ModelPathsMissing { paths: Vec<String> },
    /// No input file was found in any searched path.
    ProjectInputsEmpty {
        project_name: String,
        searched: Vec<SettingPath>,
    },
    /// A default path that is not written in `dbt_project.yml` does not
    /// exist. This is normal for optional directories.
    DefaultInputPathMissing { setting: PathSetting, path: String },
}

impl ConfigDiagnostic {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ConfiguredInputPathMissing { .. } => "configured_input_path_missing",
            Self::ModelPathsMissing { .. } => "model_paths_missing",
            Self::ProjectInputsEmpty { .. } => "project_inputs_empty",
            Self::DefaultInputPathMissing { .. } => "default_input_path_missing",
        }
    }

    pub fn level(&self) -> ConfigDiagnosticLevel {
        match self {
            Self::DefaultInputPathMissing { .. } => ConfigDiagnosticLevel::Info,
            _ => ConfigDiagnosticLevel::Warning,
        }
    }
}

/// Express `path` relative to `base` with `/` separators, using `..` for
/// paths outside `base`. Both paths are expected to be absolute and
/// normalised. When they share no common root (e.g. different Windows
/// drives), `path` is returned as is.
pub fn relative_display_path(path: &Path, base: &Path) -> String {
    let path_components: Vec<Component<'_>> = path.components().collect();
    let base_components: Vec<Component<'_>> = base.components().collect();
    let common = path_components
        .iter()
        .zip(&base_components)
        .take_while(|(a, b)| a == b)
        .count();
    let shares_root = common > 0
        && path_components
            .first()
            .is_some_and(|c| matches!(c, Component::Prefix(_) | Component::RootDir));
    if !shares_root {
        return to_slash(path);
    }
    let mut relative = PathBuf::new();
    for component in &base_components[common..] {
        if matches!(component, Component::Normal(_)) {
            relative.push("..");
        }
    }
    for component in &path_components[common..] {
        relative.push(component.as_os_str());
    }
    if relative.as_os_str().is_empty() {
        return ".".to_string();
    }
    to_slash(&relative)
}

fn to_slash(path: &Path) -> String {
    use path_slash::PathExt as _;
    path.to_slash_lossy().into_owned()
}

#[cfg(test)]
mod tests;
