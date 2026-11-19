//! Resolve manifest resource paths against the dbt project or an installed package.

use std::path::{Component, Path, PathBuf};

use super::project::DbtProject;

pub struct PackageRoots<'a> {
    project_dir: &'a Path,
    project: &'a DbtProject,
    install_dir: Option<PathBuf>,
}

pub struct ResourceRoot {
    /// The canonical root can be outside the project for a local dbt package.
    pub path: PathBuf,
    /// The installation path retains the package name and symlink location.
    pub display_path: PathBuf,
}

impl<'a> PackageRoots<'a> {
    pub fn new(project_dir: &'a Path, project: &'a DbtProject) -> Self {
        Self {
            project_dir,
            project,
            install_dir: project.package_install_dir(project_dir),
        }
    }

    pub fn resolve(&self, owner: Option<&str>) -> Result<ResourceRoot, &'static str> {
        let installed = owner.filter(|name| *name != self.project.name);
        let display_path = if let Some(name) = installed {
            let mut parts = Path::new(name).components();
            if !matches!(parts.next(), Some(Component::Normal(part)) if part == name)
                || parts.next().is_some()
            {
                return Err("invalid_package_name");
            }
            let install_dir = self.install_dir.as_ref().ok_or("unresolved_install_path")?;
            install_dir.join(name)
        } else {
            self.project_dir.to_path_buf()
        };
        let path = display_path.canonicalize().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound && installed.is_some() {
                "package_not_installed"
            } else {
                "unreadable_resource_root"
            }
        })?;
        // Opening the directory also checks that the current process can list it.
        std::fs::read_dir(&path).map_err(|_| "unreadable_resource_root")?;
        Ok(ResourceRoot { path, display_path })
    }
}

pub fn relative_resource_path(path: &str) -> Option<&Path> {
    let path = Path::new(path);
    (path
        .components()
        .any(|component| matches!(component, Component::Normal(_)))
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir)))
    .then_some(path)
}
