use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    #[serde(default, rename = "workspacePolicy")]
    pub workspace_policy: crate::worktrees::Policy,
    #[serde(default, rename = "locationIds")]
    pub location_ids: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    pub id: String,
    pub host_id: String,
    pub path: PathBuf,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub roots: Option<Vec<PathBuf>>,
    #[serde(default)]
    pub default_models: HashMap<String, String>,
    /// L-07 saved default; absent keeps the built-in default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeouts: Option<crate::timeouts::Policy>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct CatalogData {
    pub projects: Vec<Project>,
    pub locations: Vec<Location>,
    #[serde(default)]
    pub settings: Settings,
}
pub struct Catalog {
    path: PathBuf,
    pub data: CatalogData,
}
impl Catalog {
    pub fn open(path: PathBuf) -> io::Result<Self> {
        let data = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => CatalogData::default(),
            Err(e) => return Err(e),
        };
        Ok(Self { path, data })
    }
    fn save(&mut self, data: CatalogData) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = self.path.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(&data)?)?;
        std::fs::rename(temporary, &self.path)?;
        self.data = data;
        Ok(())
    }
    pub fn settings(&mut self, settings: Settings) -> io::Result<()> {
        let mut data = self.data.clone();
        data.settings = settings;
        self.save(data)
    }
    pub fn contains(&self, id: &str) -> bool {
        self.data.projects.iter().any(|p| p.id == id)
    }
    pub fn project(&mut self, id: Option<&str>, name: &str) -> Result<Project, String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 80 {
            return Err("Use a project name between 1 and 80 characters".into());
        }
        if name.eq_ignore_ascii_case("ungrouped") {
            return Err("Ungrouped is reserved for sessions without a project".into());
        }
        if self
            .data
            .projects
            .iter()
            .any(|p| Some(p.id.as_str()) != id && p.name.to_lowercase() == name.to_lowercase())
        {
            return Err("A project with that name already exists".into());
        }
        let mut data = self.data.clone();
        let project = if let Some(id) = id {
            let project = data
                .projects
                .iter_mut()
                .find(|p| p.id == id)
                .ok_or("Project not found")?;
            project.name = name.into();
            project.clone()
        } else {
            let project = Project {
                id: Uuid::new_v4().to_string(),
                name: name.into(),
                location_ids: Vec::new(),
                workspace_policy: Default::default(),
            };
            data.projects.push(project.clone());
            project
        };
        self.save(data)
            .map_err(|_| "Could not save project metadata")?;
        Ok(project)
    }
    pub fn workspace_policy(
        &mut self,
        id: &str,
        policy: crate::worktrees::Policy,
    ) -> Result<(), String> {
        let mut data = self.data.clone();
        data.projects
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or("Project not found")?
            .workspace_policy = policy;
        self.save(data)
            .map_err(|_| "Could not save workspace policy".into())
    }
    pub fn remove(&mut self, id: &str) -> Result<(), String> {
        if !self.contains(id) {
            return Err("Project not found".into());
        }
        let mut data = self.data.clone();
        data.projects.retain(|p| p.id != id);
        self.save(data)
            .map_err(|_| "Could not save project metadata".into())
    }
    pub fn attach_location(&mut self, project_id: &str, location_id: &str) -> io::Result<()> {
        let mut data = self.data.clone();
        if let Some(project) = data.projects.iter_mut().find(|p| p.id == project_id) {
            if !project.location_ids.iter().any(|id| id == location_id) {
                project.location_ids.push(location_id.into());
                self.save(data)?;
            }
        }
        Ok(())
    }
    pub fn location(&mut self, path: &Path) -> io::Result<Location> {
        if let Some(location) = self
            .data
            .locations
            .iter()
            .find(|l| l.host_id == "local" && l.path == path)
        {
            return Ok(location.clone());
        }
        let location = Location {
            id: Uuid::new_v4().to_string(),
            host_id: "local".into(),
            path: path.into(),
        };
        let mut data = self.data.clone();
        data.locations.push(location.clone());
        self.save(data)?;
        Ok(location)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projects_are_independent_of_locations_and_survive_reload() {
        let path = std::env::temp_dir().join(format!("agentworks-catalog-{}.json", Uuid::new_v4()));
        let mut catalog = Catalog::open(path.clone()).unwrap();
        let project = catalog.project(None, "App").unwrap();
        let first = catalog.location(Path::new("/projects/app")).unwrap();
        let second = catalog
            .location(Path::new("/projects/app-worktree"))
            .unwrap();
        assert_ne!(first.id, second.id);
        assert_eq!(
            catalog.location(Path::new("/projects/app")).unwrap().id,
            first.id
        );
        catalog.project(Some(&project.id), "Renamed").unwrap();
        catalog
            .settings(Settings {
                roots: Some(vec![PathBuf::from("/projects")]),
                default_models: HashMap::from([("codex".into(), "example-model".into())]),
                timeouts: None,
            })
            .unwrap();
        let mut reopened = Catalog::open(path.clone()).unwrap();
        assert_eq!(reopened.data.projects[0].name, "Renamed");
        assert_eq!(
            reopened.data.settings.default_models["codex"],
            "example-model"
        );
        assert_eq!(
            reopened.data.settings.roots.as_ref().unwrap(),
            &vec![PathBuf::from("/projects")]
        );
        assert_eq!(reopened.data.locations.len(), 2);
        reopened.remove(&project.id).unwrap();
        assert_eq!(reopened.data.locations.len(), 2);
        std::fs::remove_file(path).unwrap();
    }
}
