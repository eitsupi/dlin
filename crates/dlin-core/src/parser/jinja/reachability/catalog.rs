use std::collections::HashMap;
use std::sync::{
    OnceLock,
    atomic::{AtomicUsize, Ordering},
};

use minijinja::Environment;

use super::super::source::{ModelMacroSpan, model_macro_definition_spans};

pub(super) const RUNTIME_SCALAR_NAMES: [&str; 4] =
    ["execute", "dbt_version", "invocation_id", "run_started_at"];

/// Immutable project-level macro prefix state shared by model extraction.
/// Definition spans are collected eagerly, while MiniJinja free-symbol
/// analysis is initialized only when a runtime reachability query needs it.
#[derive(Debug)]
pub(crate) struct PreparedMacroPrefix {
    source: String,
    spans: Vec<ModelMacroSpan>,
    catalog: OnceLock<Option<PrefixCatalog>>,
    catalog_initializations: AtomicUsize,
}

#[derive(Debug)]
pub(super) struct PrefixCatalog {
    pub(super) definitions: Vec<PrefixMacroDefinition>,
    pub(super) definitions_by_name: HashMap<String, Vec<usize>>,
}

#[derive(Debug)]
pub(super) struct PrefixMacroDefinition {
    span: ModelMacroSpan,
    analysis: OnceLock<Option<PrefixMacroAnalysis>>,
}

#[derive(Debug)]
pub(super) struct PrefixMacroAnalysis {
    pub(super) free_symbols: std::collections::HashSet<String>,
    pub(super) uses_runtime_scalar: bool,
}

impl PreparedMacroPrefix {
    pub(crate) fn new(source: &str) -> Self {
        Self {
            source: source.to_owned(),
            spans: model_macro_definition_spans(source),
            catalog: OnceLock::new(),
            catalog_initializations: AtomicUsize::new(0),
        }
    }

    pub(crate) fn source(&self) -> &str {
        &self.source
    }

    pub(crate) fn spans(&self) -> &[ModelMacroSpan] {
        &self.spans
    }

    #[cfg(test)]
    pub(crate) fn catalog_initializations(&self) -> usize {
        self.catalog_initializations.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn initialized_definition_count(&self) -> usize {
        self.catalog
            .get()
            .and_then(Option::as_ref)
            .map_or(0, |catalog| {
                catalog
                    .definitions
                    .iter()
                    .filter(|definition| definition.analysis.get().is_some())
                    .count()
            })
    }

    pub(super) fn catalog(&self) -> Option<&PrefixCatalog> {
        self.catalog
            .get_or_init(|| {
                self.catalog_initializations.fetch_add(1, Ordering::Relaxed);
                build_prefix_catalog(&self.spans)
            })
            .as_ref()
    }
}

fn build_prefix_catalog(spans: &[ModelMacroSpan]) -> Option<PrefixCatalog> {
    let mut definitions = Vec::with_capacity(spans.len());
    for span in spans {
        definitions.push(PrefixMacroDefinition {
            span: span.clone(),
            analysis: OnceLock::new(),
        });
    }
    let mut definitions_by_name = HashMap::new();
    for (index, definition) in definitions.iter().enumerate() {
        definitions_by_name
            .entry(definition.span.name.clone())
            .or_insert_with(Vec::new)
            .push(index);
    }
    Some(PrefixCatalog {
        definitions,
        definitions_by_name,
    })
}

impl PrefixMacroDefinition {
    pub(super) fn analysis(&self, source: &str) -> Option<&PrefixMacroAnalysis> {
        self.analysis
            .get_or_init(|| build_prefix_macro_analysis(source, &self.span))
            .as_ref()
    }
}

fn build_prefix_macro_analysis(source: &str, span: &ModelMacroSpan) -> Option<PrefixMacroAnalysis> {
    let definition = source.get(span.start..span.end)?;
    let env = Environment::new();
    let template = env.template_from_str(definition).ok()?;
    let free_symbols = template.undeclared_variables(false);
    let uses_runtime_scalar = free_symbols
        .iter()
        .any(|symbol| RUNTIME_SCALAR_NAMES.contains(&symbol.as_str()));
    Some(PrefixMacroAnalysis {
        free_symbols,
        uses_runtime_scalar,
    })
}
