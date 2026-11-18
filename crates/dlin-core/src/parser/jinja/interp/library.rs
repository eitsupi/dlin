//! Project macros, parsed once per project and shared by every extraction.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::convert::parse_template;
use super::ir::{MacroDef, SourceId, Stmt};

/// The macros defined in the project's macro files.
#[derive(Debug, Default)]
pub(crate) struct MacroLibrary {
    macros: HashMap<Arc<str>, Arc<MacroDef>>,
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
            match parse_template(source, id) {
                Ok(template) => {
                    for stmt in template.body {
                        if let Stmt::Macro(def) = stmt {
                            library.macros.entry(def.name.clone()).or_insert(def);
                        }
                    }
                }
                Err(_) => library.unparsed.push(path.clone()),
            }
        }
        library
    }

    pub(crate) fn get(&self, name: &str) -> Option<&Arc<MacroDef>> {
        self.macros.get(name)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Arc<MacroDef>> {
        self.macros.values()
    }
}
