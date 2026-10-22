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
    #[serde(
        default,
        rename = "defaultCommunicationRole",
        skip_serializing_if = "Option::is_none"
    )]
    pub default_communication_role: Option<crate::communication::Role>,
    #[serde(default, rename = "locationIds")]
    pub location_ids: Vec<String>,
    #[serde(default)]
    pub archived: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    pub id: String,
    pub host_id: String,
    pub path: PathBuf,
}
pub use agentworks_runtime::remote::{DEFAULT_REMOTE_COMMAND, Host};
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub roots: Option<Vec<PathBuf>>,
    #[serde(default)]
    pub default_models: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_communication_role: Option<crate::communication::Role>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_default_permission_mode: Option<crate::permission_modes::ClaudeDefault>,
    /// L-07 saved default; absent keeps the built-in default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeouts: Option<crate::timeouts::Policy>,
    /// U-02 saved notification policy; absent keeps the default (off).
    #[serde(default)]
    pub notifications: crate::notifications::Settings,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct CatalogData {
    pub projects: Vec<Project>,
    pub locations: Vec<Location>,
    #[serde(default)]
    pub hosts: Vec<Host>,
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
    /// U-02 updates only the notification policy, leaving other settings untouched.
    pub fn notifications(
        &mut self,
        notifications: crate::notifications::Settings,
    ) -> io::Result<()> {
        let mut data = self.data.clone();
        data.settings.notifications = notifications;
        self.save(data)
    }
    pub fn contains(&self, id: &str) -> bool {
        self.data.projects.iter().any(|p| p.id == id)
    }
    pub fn communication_role(&self, project_id: Option<&str>) -> crate::communication::Role {
        project_id
            .and_then(|id| self.data.projects.iter().find(|p| p.id == id))
            .and_then(|p| p.default_communication_role)
            .or(self.data.settings.default_communication_role)
            .unwrap_or_default()
    }
    pub fn project(
        &mut self,
        id: Option<&str>,
        name: &str,
        role: Option<Option<crate::communication::Role>>,
    ) -> Result<Project, String> {
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
            if let Some(role) = role {
                project.default_communication_role = role;
            }
            project.clone()
        } else {
            let project = Project {
                id: Uuid::new_v4().to_string(),
                name: name.into(),
                location_ids: Vec::new(),
                workspace_policy: Default::default(),
                archived: false,
                default_communication_role: role.flatten(),
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
    pub fn set_archived(&mut self, id: &str, archived: bool) -> Result<Project, String> {
        let mut data = self.data.clone();
        let project = data
            .projects
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or("Project not found")?;
        project.archived = archived;
        let project = project.clone();
        self.save(data)
            .map_err(|_| "Could not save project metadata")?;
        Ok(project)
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
        self.location_on("local", path)
    }
    pub fn location_on(&mut self, host_id: &str, path: &Path) -> io::Result<Location> {
        if let Some(location) = self
            .data
            .locations
            .iter()
            .find(|l| l.host_id == host_id && l.path == path)
        {
            return Ok(location.clone());
        }
        let location = Location {
            id: Uuid::new_v4().to_string(),
            host_id: host_id.into(),
            path: path.into(),
        };
        let mut data = self.data.clone();
        data.locations.push(location.clone());
        self.save(data)?;
        Ok(location)
    }
    pub fn credentials_dir(&self) -> PathBuf {
        self.path
            .parent()
            .unwrap_or(Path::new("."))
            .join("host-keys")
    }
    pub fn add_host(
        &mut self,
        name: &str,
        destination: &str,
        command: &str,
    ) -> Result<Host, String> {
        self.add_managed_host(name, destination, command, None, None)
    }
    pub fn add_managed_host(
        &mut self,
        name: &str,
        destination: &str,
        command: &str,
        identity_file: Option<PathBuf>,
        known_hosts_file: Option<PathBuf>,
    ) -> Result<Host, String> {
        let mut host = self.prepare_host(name, destination, command)?;
        host.identity_file = identity_file;
        host.known_hosts_file = known_hosts_file;
        let mut data = self.data.clone();
        data.hosts.push(host.clone());
        self.save(data).map_err(|_| "Could not save host")?;
        Ok(host)
    }
    pub fn prepare_host(
        &self,
        name: &str,
        destination: &str,
        command: &str,
    ) -> Result<Host, String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 80 {
            return Err("Use a host name between 1 and 80 characters".into());
        }
        if name.eq_ignore_ascii_case("local") {
            return Err("Local is reserved for this machine".into());
        }
        if self
            .data
            .hosts
            .iter()
            .any(|h| h.name.to_lowercase() == name.to_lowercase())
        {
            return Err("A host with that name already exists".into());
        }
        let destination = destination.trim();
        // Passed to ssh as an argument, so refuse anything ssh could read as an option.
        if destination.is_empty()
            || destination.len() > 255
            || destination.starts_with('-')
            || !destination
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-@:/[]%".contains(c))
        {
            return Err("Use an SSH destination such as user@host, a ~/.ssh/config alias, or ssh://user@host:port".into());
        }
        let command = match command.trim() {
            "" => DEFAULT_REMOTE_COMMAND,
            c => c,
        };
        if command.len() > 400 || command.chars().any(char::is_control) {
            return Err("Use a single-line remote command under 400 characters".into());
        }
        let host = Host {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            destination: destination.into(),
            identity_file: None,
            known_hosts_file: None,
            command: command.into(),
            roots: Vec::new(),
        };
        Ok(host)
    }
    pub fn set_host_roots(&mut self, id: &str, roots: Vec<String>) -> Result<Host, String> {
        let mut cleaned: Vec<String> = Vec::new();
        for root in roots {
            if root.trim().is_empty() {
                return Err("Working directory cannot be empty".into());
            }
            let root = root.trim().trim_end_matches('/').to_string();
            let root = if root.is_empty() {
                "/".to_string()
            } else {
                root
            };
            if !(root.starts_with('/') || root == "~" || root.starts_with("~/"))
                || root.len() > 4096
                || root.chars().any(char::is_control)
            {
                return Err(format!("Use absolute paths or ~/… paths, not {root}"));
            }
            if !cleaned.contains(&root) {
                cleaned.push(root);
            }
        }
        if cleaned.len() > 50 {
            return Err("Use at most 50 working directories per host".into());
        }
        let mut data = self.data.clone();
        let host = data
            .hosts
            .iter_mut()
            .find(|h| h.id == id)
            .ok_or("Host not found")?;
        host.roots = cleaned;
        let host = host.clone();
        self.save(data).map_err(|_| "Could not save host")?;
        Ok(host)
    }
    /// Removes a host and the locations recorded on it. Callers must first
    /// make sure no sessions remain there.
    pub fn remove_host(&mut self, id: &str) -> Result<(), String> {
        if !self.data.hosts.iter().any(|h| h.id == id) {
            return Err("Host not found".into());
        }
        let mut data = self.data.clone();
        let gone: Vec<String> = data
            .locations
            .iter()
            .filter(|l| l.host_id == id)
            .map(|l| l.id.clone())
            .collect();
        data.locations.retain(|l| l.host_id != id);
        for project in &mut data.projects {
            project.location_ids.retain(|l| !gone.contains(l));
        }
        data.hosts.retain(|h| h.id != id);
        self.save(data).map_err(|_| "Could not save host".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn communication_defaults_inherit_and_survive_reload() {
        use crate::communication::Role;
        let path = std::env::temp_dir().join(format!("agentworks-roles-{}.json", Uuid::new_v4()));
        let mut catalog = Catalog::open(path.clone()).unwrap();
        let project = catalog.project(None, "Inherited", None).unwrap();
        assert_eq!(
            catalog.communication_role(Some(&project.id)),
            Role::Participant
        );
        let access = Role::default().access();
        assert!(access.communicate);
        assert!(
            !access.start_sessions
                && !access.prompt_sessions
                && !access.cancel_sessions
                && !access.run_automations
                && !access.restart_service
                && !access.manage_review_queue
        );
        catalog
            .settings(Settings {
                default_communication_role: Some(Role::Coordinator),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            catalog.communication_role(Some(&project.id)),
            Role::Coordinator
        );
        catalog
            .project(Some(&project.id), "Override", Some(Some(Role::Off)))
            .unwrap();
        catalog.project(Some(&project.id), "Renamed", None).unwrap();
        let mut catalog = Catalog::open(path.clone()).unwrap();
        assert_eq!(catalog.communication_role(None), Role::Coordinator);
        assert_eq!(catalog.communication_role(Some(&project.id)), Role::Off);
        catalog
            .project(Some(&project.id), "Inherited again", Some(None))
            .unwrap();
        assert_eq!(
            catalog.communication_role(Some(&project.id)),
            Role::Coordinator
        );
        let access = Role::Coordinator.access();
        assert!(
            access.communicate
                && access.start_sessions
                && access.prompt_sessions
                && access.cancel_sessions
                && access.run_automations
        );
        assert!(!access.restart_service);
        assert!(serde_json::from_value::<Role>(serde_json::json!("admin")).is_err());
        let legacy: Project =
            serde_json::from_value(serde_json::json!({"id":"old","name":"Legacy"})).unwrap();
        assert_eq!(legacy.default_communication_role, None);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn claude_permission_default_is_compatible_and_persistent() {
        use crate::permission_modes::ClaudeDefault;
        let legacy: CatalogData = serde_json::from_value(serde_json::json!({
            "projects":[], "locations":[], "settings":{"roots":null,"defaultModels":{"codex":"unchanged"}}
        })).unwrap();
        assert_eq!(
            legacy
                .settings
                .claude_default_permission_mode
                .unwrap_or_default(),
            ClaudeDefault::Auto
        );
        assert_eq!(
            Settings::default()
                .claude_default_permission_mode
                .unwrap_or_default(),
            ClaudeDefault::Auto
        );
        let path =
            std::env::temp_dir().join(format!("agentworks-claude-setting-{}.json", Uuid::new_v4()));
        let mut catalog = Catalog::open(path.clone()).unwrap();
        let mut settings = legacy.settings;
        settings.claude_default_permission_mode = Some(ClaudeDefault::Plan);
        catalog.settings(settings).unwrap();
        let saved = Catalog::open(path.clone()).unwrap().data.settings;
        assert_eq!(
            saved.claude_default_permission_mode,
            Some(ClaudeDefault::Plan)
        );
        assert_eq!(saved.default_models["codex"], "unchanged");
        assert_eq!(
            serde_json::to_value(&saved).unwrap()["claudeDefaultPermissionMode"],
            "plan"
        );
        assert!(
            serde_json::from_value::<Settings>(
                serde_json::json!({"claudeDefaultPermissionMode":"bypassPermissions"})
            )
            .is_err()
        );
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn projects_are_independent_of_locations_and_survive_reload() {
        let path = std::env::temp_dir().join(format!("agentworks-catalog-{}.json", Uuid::new_v4()));
        let mut catalog = Catalog::open(path.clone()).unwrap();
        let project = catalog.project(None, "App", None).unwrap();
        let first = catalog.location(Path::new("/projects/app")).unwrap();
        let second = catalog
            .location(Path::new("/projects/app-worktree"))
            .unwrap();
        assert_ne!(first.id, second.id);
        assert_eq!(
            catalog.location(Path::new("/projects/app")).unwrap().id,
            first.id
        );
        catalog.project(Some(&project.id), "Renamed", None).unwrap();
        catalog.set_archived(&project.id, true).unwrap();
        catalog
            .settings(Settings {
                roots: Some(vec![PathBuf::from("/projects")]),
                default_models: HashMap::from([("codex".into(), "example-model".into())]),
                timeouts: None,
                claude_default_permission_mode: None,
                default_communication_role: None,
                notifications: Default::default(),
            })
            .unwrap();
        let mut reopened = Catalog::open(path.clone()).unwrap();
        assert_eq!(reopened.data.projects[0].name, "Renamed");
        assert!(reopened.data.projects[0].archived);
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

    #[test]
    fn remote_hosts_are_validated_and_survive_reload() {
        let path = std::env::temp_dir().join(format!("agentworks-hosts-{}.json", Uuid::new_v4()));
        let mut catalog = Catalog::open(path.clone()).unwrap();
        assert!(catalog.add_host("Local", "box", "").is_err());
        assert!(catalog.add_host("box", "-oProxyCommand=sh", "").is_err());
        assert!(catalog.add_host("box", "user@host name", "").is_err());
        assert!(
            catalog
                .add_host("box", "box", "agentworks\nrm -rf /")
                .is_err()
        );
        let host = catalog.add_host("Build box", "dev@build.lan", "").unwrap();
        assert_eq!(host.command, DEFAULT_REMOTE_COMMAND);
        assert!(catalog.add_host("build BOX", "other", "").is_err());
        let custom = catalog
            .add_host(
                "Laptop",
                "ssh://me@[fe80::1]:2222",
                "~/agentworks/target/release/agentworks",
            )
            .unwrap();
        let reopened = Catalog::open(path.clone()).unwrap();
        assert_eq!(reopened.data.hosts.len(), 2);
        assert_eq!(reopened.data.hosts[1].command, custom.command);
        assert!(
            catalog
                .set_host_roots(&host.id, vec!["relative/path".into()])
                .is_err()
        );
        let rooted = catalog
            .set_host_roots(
                &host.id,
                vec!["~/projects/".into(), "/srv/app".into(), "~/projects".into()],
            )
            .unwrap();
        assert_eq!(rooted.roots, ["~/projects", "/srv/app"]);
        let remote = catalog
            .location_on(&host.id, Path::new("/srv/app"))
            .unwrap();
        assert_ne!(
            remote.id,
            catalog.location(Path::new("/srv/app")).unwrap().id
        );
        let project = catalog.project(None, "Spans hosts", None).unwrap();
        catalog.attach_location(&project.id, &remote.id).unwrap();
        catalog.remove_host(&host.id).unwrap();
        assert!(catalog.remove_host(&host.id).is_err());
        assert!(catalog.data.locations.iter().all(|l| l.host_id != host.id));
        assert!(catalog.data.projects[0].location_ids.is_empty());
        assert_eq!(catalog.data.hosts.len(), 1);
        std::fs::remove_file(path).unwrap();
    }
}
