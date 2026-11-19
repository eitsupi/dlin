//! The columns of seeds, read from the seed files of a project.
//!
//! A seed's columns are the header of its file. When a project's YAML
//! declares no columns for a seed, the header is the only record of them in
//! the project: without it, every name the seed may carry competes with the
//! relations a model joins it with. Only seeds of the project itself are read,
//! from files inside the project directory.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use crate::parser::manifest::{Manifest, ManifestNode};
use crate::parser::project::DbtProject;

/// The header columns of the seeds of one project, by seed unique id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeedSchemas {
    columns: HashMap<String, SeedFileColumns>,
}

/// The columns one seed file lists, in order, with the file they were read
/// from relative to the project directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeedFileColumns {
    pub(crate) path: String,
    pub(crate) names: Vec<String>,
}

/// A seed file that exists but whose header gives no column list to rely
/// on. The seed is then treated as if no file had been read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedFileWarning {
    pub unique_id: String,
    pub path: PathBuf,
    pub reason: String,
}

impl SeedSchemas {
    /// Read the seed files of the project in `project_dir` for the seeds of
    /// `manifest` that declare no columns.
    ///
    /// Nothing is read unless `project_dir` holds the `dbt_project.yml` of the
    /// project the manifest was built for, so a manifest analysed away from
    /// its project never reads unrelated files. Seeds of installed packages
    /// are not read. A file that is missing is skipped silently; a file whose
    /// header cannot be used is skipped with a warning.
    pub fn read(project_dir: &Path, manifest: &Manifest) -> (Self, Vec<SeedFileWarning>) {
        let mut schemas = Self::default();
        let mut warnings = Vec::new();
        let Ok(project) = DbtProject::load(project_dir) else {
            return (schemas, warnings);
        };
        if manifest.metadata.project_name.as_deref() != Some(project.name.as_str()) {
            return (schemas, warnings);
        }
        let Ok(root) = project_dir.canonicalize() else {
            return (schemas, warnings);
        };
        let mut seeds: Vec<&ManifestNode> = manifest
            .nodes
            .values()
            .filter(|node| {
                node.resource_type == "seed"
                    && node.columns.is_empty()
                    && seed_package(&node.unique_id) == Some(project.name.as_str())
            })
            .collect();
        seeds.sort_by(|left, right| left.unique_id.cmp(&right.unique_id));
        for node in seeds {
            let Some(relative) = node.original_file_path.as_deref() else {
                continue;
            };
            let Some(path) = file_in_project(&root, relative) else {
                continue;
            };
            if !is_csv(Path::new(relative)) {
                continue;
            }
            match read_header(&path, node.config.delimiter.as_deref()) {
                Ok(names) => {
                    schemas.columns.insert(
                        node.unique_id.clone(),
                        SeedFileColumns {
                            path: relative.to_owned(),
                            names,
                        },
                    );
                }
                Err(reason) => warnings.push(SeedFileWarning {
                    unique_id: node.unique_id.clone(),
                    path,
                    reason,
                }),
            }
        }
        (schemas, warnings)
    }

    /// The columns read for the seed `unique_id`.
    pub(crate) fn get(&self, unique_id: &str) -> Option<&SeedFileColumns> {
        self.columns.get(unique_id)
    }
}

/// The package of a seed, from its unique id `seed.<package>.<name>`.
fn seed_package(unique_id: &str) -> Option<&str> {
    let mut parts = unique_id.splitn(3, '.');
    (parts.next() == Some("seed"))
        .then(|| parts.next())
        .flatten()
}

/// The file `relative` names inside `root`, when it names one there: a
/// relative path that does not climb out of the root, and that still lies
/// inside it once links are resolved.
fn file_in_project(root: &Path, relative: &str) -> Option<PathBuf> {
    let relative = Path::new(relative);
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }
    let path = root.join(relative).canonicalize().ok()?;
    (path.starts_with(root) && path.is_file()).then_some(path)
}

fn is_csv(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"))
}

/// The column names in the header of the CSV file at `path`.
///
/// Every name must be present and distinct even after case folding. dbt renames empty and
/// repeated headers when it loads a seed; the renamed columns are not
/// reproduced here, so such a header is rejected.
fn read_header(path: &Path, delimiter: Option<&str>) -> Result<Vec<String>, String> {
    let delimiter = match delimiter {
        None => b',',
        Some(delimiter) => match delimiter.as_bytes() {
            [byte] if byte.is_ascii() && !matches!(byte, b'\0' | b'\r' | b'\n' | b'"') => *byte,
            _ => return Err(format!("unsupported delimiter {delimiter:?}")),
        },
    };
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(true)
        .flexible(true)
        .from_path(path)
        .map_err(|error| format!("cannot read the file: {error}"))?;
    let header = reader
        .headers()
        .map_err(|error| format!("cannot read the header: {error}"))?;
    if header.is_empty() {
        return Err("the header lists no columns".to_owned());
    }
    let mut seen = HashSet::new();
    let mut names = Vec::with_capacity(header.len());
    for name in header {
        if name.trim().is_empty() {
            return Err("the header has an empty column name".to_owned());
        }
        if !seen.insert(name.to_lowercase()) {
            return Err(format!("the header repeats the column name {name:?}"));
        }
        names.push(name.to_owned());
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn header(content: &str, delimiter: Option<&str>) -> Result<Vec<String>, String> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("seed.csv");
        fs::write(&path, content).unwrap();
        read_header(&path, delimiter)
    }

    #[test]
    fn header_names_are_read_in_order() {
        assert_eq!(
            header("source,source_category\na,b\n", None).unwrap(),
            ["source", "source_category"]
        );
        // A byte order mark, a quoted name and a header-only file.
        assert_eq!(header("\u{feff}\"a,b\",c\n", None).unwrap(), ["a,b", "c"]);
        assert_eq!(header("a|b\n1|2\n", Some("|")).unwrap(), ["a", "b"]);
    }

    #[test]
    fn unusable_headers_are_rejected() {
        assert!(header("", None).is_err());
        assert!(header("a,,b\n", None).is_err());
        assert!(header("a, \n", None).is_err());
        assert!(header("x,X\n", None).is_err());
        assert!(header("é,É\n", None).is_err());
        assert!(header("a,b\n", Some("||")).is_err());
    }

    #[test]
    fn seed_package_comes_from_the_unique_id() {
        assert_eq!(
            seed_package("seed.my_project.categories"),
            Some("my_project")
        );
        assert_eq!(seed_package("model.my_project.categories"), None);
    }

    #[test]
    fn files_outside_the_project_are_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("seeds")).unwrap();
        fs::write(root.join("seeds/a.csv"), "x\n").unwrap();
        assert!(file_in_project(&root, "seeds/a.csv").is_some());
        assert!(file_in_project(&root, "seeds/missing.csv").is_none());
        assert!(file_in_project(&root, "../a.csv").is_none());
        assert!(file_in_project(&root, "/etc/hosts").is_none());
    }
}
