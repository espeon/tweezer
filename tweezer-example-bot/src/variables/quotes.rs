use std::time::{SystemTime, UNIX_EPOCH};

use crate::db::Database;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Quote {
    pub id: usize,
    pub text: String,
    pub author: String,
    pub timestamp: u64,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct QuotesDb {
    db: Option<Database>,
}

impl QuotesDb {
    pub fn new() -> Self {
        Self { db: None }
    }

    pub fn with_db(mut self, db: Database) -> Self {
        self.db = Some(db);
        self
    }

    pub fn add(&self, text: &str, author: &str) -> usize {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        match self.db {
            Some(ref db) => db.quote_add(text, author, timestamp).unwrap_or(0),
            None => 0,
        }
    }

    pub fn get(&self, id: usize) -> Option<Quote> {
        self.db.as_ref()?.quote_get(id).ok()?.map(|(id, text, author, timestamp)| {
            Quote { id, text, author, timestamp }
        })
    }

    pub fn random(&self) -> Option<Quote> {
        self.db.as_ref()?.quote_random().ok()?.map(|(id, text, author, timestamp)| {
            Quote { id, text, author, timestamp }
        })
    }

    pub fn remove(&self, id: usize) -> bool {
        match self.db {
            Some(ref db) => db.quote_remove(id).unwrap_or(false),
            None => false,
        }
    }

    pub fn count(&self) -> usize {
        match self.db {
            Some(ref db) => db.quote_count().unwrap_or(0),
            None => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn make_db() -> QuotesDb {
        QuotesDb::new().with_db(Database::open_in_memory().unwrap())
    }

    #[test]
    fn add_and_get() {
        let db = make_db();
        let id = db.add("hello world", "alice");
        assert_eq!(id, 1);
        let q = db.get(1).unwrap();
        assert_eq!(q.text, "hello world");
        assert_eq!(q.author, "alice");
    }

    #[test]
    fn remove_renumbers() {
        let db = make_db();
        db.add("first", "a");
        db.add("second", "b");
        db.add("third", "c");
        assert!(db.remove(2));
        assert_eq!(db.count(), 2);
        assert_eq!(db.get(1).unwrap().text, "first");
        assert_eq!(db.get(2).unwrap().text, "third");
    }

    #[test]
    fn remove_invalid_returns_false() {
        let db = make_db();
        assert!(!db.remove(99));
    }

    #[test]
    fn random_empty_returns_none() {
        let db = make_db();
        assert!(db.random().is_none());
    }
}
