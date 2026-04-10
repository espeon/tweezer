use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;

pub trait CommandStore: Send + Sync {
    fn load(&self) -> io::Result<HashMap<String, String>>;
    fn save(&self, commands: &HashMap<String, String>) -> io::Result<()>;
}

pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl CommandStore for FileStore {
    fn load(&self) -> io::Result<HashMap<String, String>> {
        if !self.path.exists() {
            return Ok(HashMap::new());
        }
        let data = fs::read_to_string(&self.path)?;
        let map: HashMap<String, String> = serde_json::from_str(&data).unwrap_or_default();
        Ok(map)
    }

    fn save(&self, commands: &HashMap<String, String>) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }
        let data = serde_json::to_string_pretty(commands)?;
        fs::write(&self.path, data)
    }
}

pub struct NullStore;

impl CommandStore for NullStore {
    fn load(&self) -> io::Result<HashMap<String, String>> {
        Ok(HashMap::new())
    }

    fn save(&self, _commands: &HashMap<String, String>) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn file_store_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commands.json");
        let store = FileStore::new(&path);

        let mut commands = HashMap::new();
        commands.insert("hello".to_string(), "Hello, $(user)!".to_string());
        commands.insert("dice".to_string(), "$(random 1-6)".to_string());

        store.save(&commands).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded, commands);
    }

    #[test]
    fn file_store_load_missing_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        let store = FileStore::new(&path);
        let loaded = store.load().unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn file_store_load_corrupt_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commands.json");
        fs::write(&path, "not valid json{{{{").unwrap();
        let store = FileStore::new(&path);
        let loaded = store.load().unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn null_store_does_nothing() {
        let store = NullStore;
        let mut commands = HashMap::new();
        commands.insert("test".to_string(), "value".to_string());
        store.save(&commands).unwrap();
        let loaded = store.load().unwrap();
        assert!(loaded.is_empty());
    }
}
