use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use iris_llm::Message;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub messages: Vec<Message>,
    pub created_at: u64,
    pub updated_at: u64,
}

pub struct Storage {
    dir: PathBuf,
}

impl Storage {
    pub fn new() -> Result<Self> {
        let home = dirs::home_dir().context("Cannot determine home directory")?;
        let dir = home.join(".code-iris").join("sessions");
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create sessions directory: {}", dir.display()))?;
        Ok(Self { dir })
    }

    pub fn save(&self, session: &Session) -> Result<()> {
        let path = self.dir.join(format!("{}.json", session.id));
        let json = serde_json::to_string_pretty(session)
            .context("Failed to serialize session")?;
        std::fs::write(&path, json)
            .with_context(|| format!("Failed to write session to {}", path.display()))?;
        Ok(())
    }

    pub fn load(&self, id: &str) -> Result<Session> {
        let path = self.dir.join(format!("{}.json", id));
        let json = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read session from {}", path.display()))?;
        let session: Session = serde_json::from_str(&json)
            .with_context(|| format!("Failed to parse session from {}", path.display()))?;
        Ok(session)
    }

    pub fn list(&self) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        let entries = std::fs::read_dir(&self.dir)
            .with_context(|| format!("Failed to read sessions directory: {}", self.dir.display()))?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if let Some(id) = name_str.strip_suffix(".json") {
                ids.push(id.to_string());
            }
        }
        ids.sort();
        Ok(ids)
    }
}

impl Session {
    pub fn new() -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            id,
            messages: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }
}

pub fn new_session() -> Session {
    Session::new()
}
