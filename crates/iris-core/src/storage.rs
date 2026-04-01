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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_storage() -> (Storage, PathBuf) {
        let dir = std::env::temp_dir().join(format!("iris_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        (Storage { dir: dir.clone() }, dir)
    }

    #[test]
    fn session_new_has_unique_ids() {
        let s1 = Session::new();
        let s2 = Session::new();
        assert_ne!(s1.id, s2.id);
        assert!(s1.messages.is_empty());
    }

    #[test]
    fn save_and_load_round_trip() {
        let (storage, dir) = temp_storage();
        let mut session = Session::new();
        session.messages.push(iris_llm::Message::user("hello"));

        storage.save(&session).unwrap();
        let loaded = storage.load(&session.id).unwrap();

        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.messages.len(), 1);

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn load_nonexistent_returns_error() {
        let (storage, dir) = temp_storage();
        let result = storage.load("no_such_id");
        assert!(result.is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn list_returns_saved_ids() {
        let (storage, dir) = temp_storage();
        let s1 = Session::new();
        let s2 = Session::new();
        storage.save(&s1).unwrap();
        storage.save(&s2).unwrap();

        let ids = storage.list().unwrap();
        assert!(ids.contains(&s1.id));
        assert!(ids.contains(&s2.id));

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn list_empty_directory() {
        let (storage, dir) = temp_storage();
        let ids = storage.list().unwrap();
        assert!(ids.is_empty());
        std::fs::remove_dir_all(dir).ok();
    }
}
