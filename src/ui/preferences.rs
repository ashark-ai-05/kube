//! Small local preferences; no cluster credentials or resource contents are saved.
use std::path::PathBuf;
pub struct Preferences {
    pub sidebar: u16,
    pub inspector: u16,
    pub mouse: bool,
    path: PathBuf,
}
impl Default for Preferences {
    fn default() -> Self {
        Self::load()
    }
}
impl Preferences {
    pub fn load() -> Self {
        let root = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
            });
        let path = root.join("kube/preferences.json");
        let value = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .unwrap_or_default();
        Self {
            sidebar: value["sidebar"].as_u64().unwrap_or(28).min(60) as u16,
            inspector: value["inspector"].as_u64().unwrap_or(65).clamp(30, 80) as u16,
            mouse: value["mouse"].as_bool().unwrap_or(true),
            path,
        }
    }
    pub fn save(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path,serde_json::json!({"sidebar":self.sidebar,"inspector":self.inspector,"mouse":self.mouse}).to_string())
    }
}
