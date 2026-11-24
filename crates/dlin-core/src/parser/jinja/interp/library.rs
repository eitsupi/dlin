//! Project macros, parsed once per project and shared by every extraction.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use super::convert::parse_macro_file;
use super::ir::{MacroDef, SourceId, Stmt};
use super::value::{AValue, Closure, MacroRef};
use crate::parser::project::MacroResolution;

/// The macros defined in the project's macro files.
#[derive(Debug, Default)]
pub(crate) struct MacroLibrary {
    macros: HashMap<Arc<str>, Arc<MacroDef>>,
    duplicates: HashSet<Arc<str>>,
    adapter_overrides: HashSet<Arc<str>>,
    resolution: Option<MacroResolution>,
    inventory_incomplete: bool,
    /// String literals of the expressions assigned to each variable name in
    /// macro bodies, for explaining rejected computed names.
    pub(crate) set_literals: HashMap<Arc<str>, Vec<String>>,
    /// Files that could not be parsed. Their macros are unknown.
    pub(crate) unparsed: Vec<PathBuf>,
}

impl MacroLibrary {
    /// Parse macro files. When several files define the same macro, the
    /// first in the given order is used.
    pub(crate) fn new(files: &[(PathBuf, String)]) -> Self {
        let mut library = MacroLibrary::default();
        for (index, (path, source)) in files.iter().enumerate() {
            let id = SourceId::MacroFile(u32::try_from(index).unwrap_or(u32::MAX));
            match parse_macro_file(source, id) {
                Ok(template) => {
                    super::eval::collect_set_literals(&template.body, &mut library.set_literals);
                    for stmt in template.body {
                        if let Stmt::Macro(def) = stmt {
                            if let Some((prefix, name)) = def.name.split_once("__")
                                && !prefix.is_empty()
                                && prefix != "default"
                            {
                                // An unfamiliar prefix could be an adapter. Treat even ordinary
                                // macro names in this form as possible competitors.
                                library.adapter_overrides.insert(Arc::from(name));
                            }
                            match library.macros.entry(def.name.clone()) {
                                std::collections::hash_map::Entry::Vacant(entry) => {
                                    entry.insert(def);
                                }
                                std::collections::hash_map::Entry::Occupied(_) => {
                                    library.duplicates.insert(def.name.clone());
                                }
                            }
                        }
                    }
                }
                Err(_) => library.unparsed.push(path.clone()),
            }
        }
        library
    }

    pub(crate) fn with_resolution(
        mut self,
        resolution: &MacroResolution,
        inventory_incomplete: bool,
    ) -> Self {
        self.resolution = Some(resolution.clone());
        self.inventory_incomplete = inventory_incomplete || !self.unparsed.is_empty();
        self
    }

    pub(super) fn namespace_member(&self, namespace: &str, name: &str) -> Option<AValue> {
        if self.resolution.as_ref()?.root_namespace != namespace
            || namespace == "dbt"
            || self.duplicates.contains(name)
        {
            return None;
        }
        self.get(name).map(|def| {
            AValue::Macro(MacroRef {
                def: def.clone(),
                closure: Closure::Project,
            })
        })
    }

    pub(super) fn dispatch(&self, name: &str, namespace: &str) -> Option<AValue> {
        // A named search of the root package is closed. No internal or
        // installed package can supply an earlier implementation in it.
        if self.inventory_incomplete
            || self
                .resolution
                .as_ref()?
                .dispatch_overrides
                .affects(namespace)
            || self.adapter_overrides.contains(name)
            || name.is_empty()
            || name.contains('.')
        {
            return None;
        }
        self.namespace_member(namespace, &format!("default__{name}"))
    }

    pub(crate) fn get(&self, name: &str) -> Option<&Arc<MacroDef>> {
        self.macros.get(name)
    }
}
