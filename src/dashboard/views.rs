//! Local view definitions: queries and presentation only; never cluster credentials or pod data.
use super::{
    Dashboard,
    pod::{Filter, Sort},
    query::Query,
};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Column {
    Name,
    Ready,
    Restarts,
    Age,
    Cpu,
    Memory,
    Status,
    Node,
}
impl Column {
    pub const ALL: [Self; 8] = [
        Self::Name,
        Self::Ready,
        Self::Restarts,
        Self::Age,
        Self::Cpu,
        Self::Memory,
        Self::Status,
        Self::Node,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Ready => "Ready",
            Self::Restarts => "Restarts",
            Self::Age => "Age",
            Self::Cpu => "CPU",
            Self::Memory => "Memory",
            Self::Status => "Status",
            Self::Node => "Node",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|c| c.label().eq_ignore_ascii_case(text))
    }
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|c| *c == self).unwrap()
    }
}
pub fn normalized(columns: &[Column]) -> Vec<Column> {
    Column::ALL
        .into_iter()
        .filter(|c| *c == Column::Name || columns.contains(c))
        .collect()
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub query: String,
    pub filter: String,
    pub sort: String,
    pub descending: bool,
    pub columns: Vec<Column>,
}
impl Settings {
    pub fn capture(dashboard: &Dashboard, query: &str) -> Self {
        Self {
            query: query.into(),
            filter: dashboard.filter.key().into(),
            sort: dashboard.sort.label().into(),
            descending: dashboard.descending,
            columns: dashboard.columns.clone(),
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        Query::parse(&self.query)?;
        if Filter::parse(&self.filter).is_none() || Sort::parse(&self.sort).is_none() {
            return Err("Unknown saved filter or sort".into());
        }
        Ok(())
    }
    pub fn apply(&self, dashboard: &mut Dashboard) -> Result<(), String> {
        self.validate()?;
        dashboard.search = self.query.clone();
        dashboard.set_filter(Filter::parse(&self.filter).unwrap());
        dashboard.set_sort(Sort::parse(&self.sort).unwrap(), self.descending);
        dashboard.columns = normalized(&self.columns);
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedView {
    pub name: String,
    pub settings: Settings,
}
#[derive(Clone, Serialize, Deserialize)]
struct Document {
    version: u32,
    columns: Vec<Column>,
    views: Vec<SavedView>,
}
impl Default for Document {
    fn default() -> Self {
        Self {
            version: 1,
            columns: Column::ALL.to_vec(),
            views: vec![],
        }
    }
}
pub struct Library {
    path: PathBuf,
    data: Document,
    pub error: Option<String>,
}
impl Library {
    pub fn load() -> Self {
        let root = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
            });
        Self::from_path(root.join("kube/views.json"))
    }
    fn from_path(path: PathBuf) -> Self {
        let read = || -> Result<Document, String> {
            match std::fs::metadata(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Document::default());
                }
                Err(_) => return Err("Cannot read saved views".into()),
                Ok(meta) if meta.len() > 65536 => {
                    return Err("Saved views file exceeds 64 KiB".into());
                }
                _ => {}
            }
            let bytes = std::fs::read(&path).map_err(|_| "Cannot read saved views")?;
            let mut doc: Document = serde_json::from_slice(&bytes)
                .map_err(|_| "Saved views file is invalid; it has been preserved")?;
            if doc.version != 1 || doc.views.len() > 50 {
                return Err("Unsupported saved views file; it has been preserved".into());
            }
            for view in &doc.views {
                Self::name(&view.name)?;
                view.settings.validate()?;
            }
            doc.columns = normalized(&doc.columns);
            Ok(doc)
        };
        match read() {
            Ok(data) => Self {
                path,
                data,
                error: None,
            },
            Err(error) => Self {
                path,
                data: Document::default(),
                error: Some(error),
            },
        }
    }
    fn name(name: &str) -> Result<(), String> {
        if name.is_empty() || name.len() > 80 || name.chars().any(char::is_control) {
            Err("Use a view name of 1–80 bytes without control characters".into())
        } else {
            Ok(())
        }
    }
    pub fn views(&self) -> &[SavedView] {
        &self.data.views
    }
    pub fn columns(&self) -> Vec<Column> {
        self.data.columns.clone()
    }
    pub fn find(&self, name: &str) -> Option<&SavedView> {
        self.data.views.iter().find(|v| v.name == name)
    }
    pub fn save_view(&mut self, name: &str, settings: Settings) -> Result<(), String> {
        Self::name(name)?;
        settings.validate()?;
        let mut data = self.data.clone();
        if let Some(view) = data.views.iter_mut().find(|v| v.name == name) {
            view.settings = settings;
        } else {
            if data.views.len() == 50 {
                return Err("Saved views are limited to 50; delete an unused view first".into());
            }
            data.views.push(SavedView {
                name: name.into(),
                settings,
            });
        }
        data.views.sort_by(|a, b| a.name.cmp(&b.name));
        self.store(data)
    }
    pub fn delete(&mut self, name: &str) -> Result<(), String> {
        if self.find(name).is_none() {
            return Err("No saved view has that name".into());
        }
        let mut data = self.data.clone();
        data.views.retain(|v| v.name != name);
        self.store(data)
    }
    pub fn set_columns(&mut self, columns: &[Column]) -> Result<(), String> {
        let mut data = self.data.clone();
        data.columns = normalized(columns);
        self.store(data)
    }
    fn store(&mut self, data: Document) -> Result<(), String> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        let bytes = serde_json::to_vec_pretty(&data).map_err(|_| "Could not encode saved views")?;
        if bytes.len() > 65536 {
            return Err("Saved views exceed 64 KiB; shorten a query or remove a view".into());
        }
        atomic_write(&self.path, &bytes).map_err(|e| format!("Could not save views: {e}"))?;
        self.data = data;
        Ok(())
    }
}
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).map_err(|e| std::io::Error::other(e.to_string()))?;
    let temp = parent.join(format!(".views-{:x}.tmp", u64::from_ne_bytes(random)));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}
pub fn columns_picker(columns: &[Column]) -> crate::ui::views::picker::Picker {
    use crate::ui::views::picker::{Picker, PickerItem};
    Picker {
        title: "Pod columns · Enter toggle · Esc done".into(),
        items: Column::ALL
            .into_iter()
            .map(|c| PickerItem {
                label: c.label().into(),
                detail: if c == Column::Name {
                    "always visible".into()
                } else if columns.contains(&c) {
                    "shown · Enter hides".into()
                } else {
                    "hidden · Enter shows".into()
                },
                accent: None,
            })
            .collect(),
        filter: String::new(),
        selected: 0,
        scroll: 0,
    }
}
pub fn views_picker(library: &Library) -> crate::ui::views::picker::Picker {
    use crate::ui::views::picker::{Picker, PickerItem};
    Picker {
        title: "Saved pod views · current cluster / namespace".into(),
        items: library
            .views()
            .iter()
            .map(|view| PickerItem {
                label: view.name.clone(),
                detail: format!(
                    "{} · {} {} · {}",
                    view.settings.filter,
                    view.settings.sort,
                    if view.settings.descending {
                        "↓"
                    } else {
                        "↑"
                    },
                    view.settings.query
                ),
                accent: None,
            })
            .collect(),
        filter: String::new(),
        selected: 0,
        scroll: 0,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn view_roundtrip_updates_deletes_and_preserves_scope_identity() {
        let dir = std::env::temp_dir().join(format!("kube-view-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("views.json");
        let mut library = Library::from_path(path.clone());
        let mut dashboard = Dashboard::default();
        dashboard.set_sort(Sort::Restarts, true);
        let settings = Settings::capture(&dashboard, "restarts>3 ready=false");
        library.save_view("Failing pods", settings).unwrap();
        library
            .set_columns(&[Column::Memory, Column::Name, Column::Name])
            .unwrap();
        let mut restored = Library::from_path(path.clone());
        assert_eq!(restored.columns(), [Column::Name, Column::Memory]);
        dashboard.anchor = Some(super::super::pod::Identity {
            namespace: "current".into(),
            name: "web".into(),
            uid: Some("same".into()),
        });
        restored
            .find("Failing pods")
            .unwrap()
            .settings
            .apply(&mut dashboard)
            .unwrap();
        assert_eq!(dashboard.anchor.as_ref().unwrap().namespace, "current");
        assert_eq!(dashboard.search, "restarts>3 ready=false");
        assert_eq!(dashboard.sort, Sort::Restarts);
        assert!(dashboard.descending);
        restored
            .save_view("Failing pods", Settings::capture(&dashboard, "restarts>5"))
            .unwrap();
        assert_eq!(restored.views().len(), 1);
        restored.delete("Failing pods").unwrap();
        assert!(Library::from_path(path.clone()).views().is_empty());
        std::fs::write(&path, b"broken").unwrap();
        let mut broken = Library::from_path(path.clone());
        assert!(
            broken
                .save_view("new", Settings::capture(&dashboard, ""))
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"broken");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
