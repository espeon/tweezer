use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};

// ---------------------------------------------------------------------------
// Database
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    pub fn open(path: &str) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.migrate()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self, rusqlite::Error> {
        let conn = Connection::open_in_memory()?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "CREATE TABLE IF NOT EXISTS dynamic_commands (
                name TEXT PRIMARY KEY,
                template TEXT NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS quotes (
                id INTEGER PRIMARY KEY,
                text TEXT NOT NULL,
                author TEXT NOT NULL,
                created_at INTEGER NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS counters (
                key TEXT PRIMARY KEY,
                count INTEGER NOT NULL DEFAULT 0
            )",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS reminders (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                channel TEXT NOT NULL,
                user_id TEXT NOT NULL,
                user_name TEXT NOT NULL,
                message TEXT NOT NULL,
                trigger_at INTEGER NOT NULL
            )",
            [],
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------
    // Dynamic commands
    // -----------------------------------------------------------------

    pub fn cmd_add(&self, name: &str, template: &str) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO dynamic_commands (name, template)
             VALUES (?1, ?2)
             ON CONFLICT(name) DO UPDATE SET template = excluded.template",
            params![name, template],
        )?;
        Ok(())
    }

    pub fn cmd_remove(&self, name: &str) -> Result<bool, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let changed = conn.execute(
            "DELETE FROM dynamic_commands WHERE name = ?1",
            params![name],
        )?;
        Ok(changed > 0)
    }

    pub fn cmd_get(&self, name: &str) -> Result<Option<String>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT template FROM dynamic_commands WHERE name = ?1")?;
        let mut rows = stmt.query(params![name])?;
        Ok(rows.next()?.map(|r| r.get(0).unwrap()))
    }

    pub fn cmd_list(&self) -> Result<Vec<(String, String)>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT name, template FROM dynamic_commands ORDER BY name")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect()
    }

    // -----------------------------------------------------------------
    // Quotes
    // -----------------------------------------------------------------

    pub fn quote_add(&self, text: &str, author: &str, created_at: u64) -> Result<usize, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO quotes (text, author, created_at) VALUES (?1, ?2, ?3)",
            params![text, author, created_at as i64],
        )?;
        Ok(conn.last_insert_rowid() as usize)
    }

    pub fn quote_get(&self, id: usize) -> Result<Option<(usize, String, String, u64)>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id, text, author, created_at FROM quotes WHERE id = ?1")?;
        let mut rows = stmt.query(params![id as i64])?;
        Ok(rows.next()?.map(|r| {
            (
                r.get::<_, i64>(0).unwrap() as usize,
                r.get(1).unwrap(),
                r.get(2).unwrap(),
                r.get::<_, i64>(3).unwrap() as u64,
            )
        }))
    }

    pub fn quote_random(&self) -> Result<Option<(usize, String, String, u64)>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, text, author, created_at FROM quotes ORDER BY RANDOM() LIMIT 1"
        )?;
        let mut rows = stmt.query([])?;
        Ok(rows.next()?.map(|r| {
            (
                r.get::<_, i64>(0).unwrap() as usize,
                r.get(1).unwrap(),
                r.get(2).unwrap(),
                r.get::<_, i64>(3).unwrap() as u64,
            )
        }))
    }

    pub fn quote_remove(&self, id: usize) -> Result<bool, rusqlite::Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "DELETE FROM quotes WHERE id = ?1",
            params![id as i64],
        )?;
        if changed > 0 {
            tx.execute(
                "UPDATE quotes SET id = id - 1 WHERE id > ?1",
                params![id as i64],
            )?;
        }
        tx.commit()?;
        Ok(changed > 0)
    }

    pub fn quote_count(&self) -> Result<usize, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM quotes",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    pub fn quote_all(&self) -> Result<Vec<(usize, String, String, u64)>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, text, author, created_at FROM quotes ORDER BY id"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)? as usize,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)? as u64,
            ))
        })?;
        rows.collect()
    }

    // -----------------------------------------------------------------
    // Counters
    // -----------------------------------------------------------------

    pub fn counter_increment(&self, key: &str) -> Result<u32, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO counters (key, count) VALUES (?1, 1)
             ON CONFLICT(key) DO UPDATE SET count = count + 1",
            params![key],
        )?;
        let count: i64 = conn.query_row(
            "SELECT count FROM counters WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )?;
        Ok(count as u32)
    }

    pub fn counter_get(&self, key: &str) -> Result<u32, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COALESCE((SELECT count FROM counters WHERE key = ?1), 0)",
            params![key],
            |row| row.get(0),
        )?;
        Ok(count as u32)
    }

    // -----------------------------------------------------------------
    // Reminders
    // -----------------------------------------------------------------

    pub fn reminder_add(
        &self,
        channel: &str,
        user_id: &str,
        user_name: &str,
        message: &str,
        trigger_at: u64,
    ) -> Result<usize, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO reminders (channel, user_id, user_name, message, trigger_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![channel, user_id, user_name, message, trigger_at as i64],
        )?;
        Ok(conn.last_insert_rowid() as usize)
    }

    pub fn reminder_remove(&self, id: usize) -> Result<bool, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let changed = conn.execute(
            "DELETE FROM reminders WHERE id = ?1",
            params![id as i64],
        )?;
        Ok(changed > 0)
    }

    pub fn reminder_due(&self, before: u64) -> Result<Vec<(usize, String, String, String, String)>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, channel, user_id, user_name, message
             FROM reminders WHERE trigger_at <= ?1
             ORDER BY trigger_at"
        )?;
        let rows = stmt.query_map(params![before as i64], |row| {
            Ok((
                row.get::<_, i64>(0)? as usize,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        rows.collect()
    }

    pub fn reminder_delete_due(&self, before: u64) -> Result<usize, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let changed = conn.execute(
            "DELETE FROM reminders WHERE trigger_at <= ?1",
            params![before as i64],
        )?;
        Ok(changed)
    }

    pub fn reminder_count(&self) -> Result<usize, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM reminders",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_commands_roundtrip() {
        let db = Database::open_in_memory().unwrap();
        db.cmd_add("hello", "Hello $(user)!").unwrap();
        db.cmd_add("dice", "$(random 1-6)").unwrap();

        assert_eq!(db.cmd_get("hello").unwrap(), Some("Hello $(user)!".into()));
        assert_eq!(db.cmd_list().unwrap().len(), 2);

        db.cmd_remove("hello").unwrap();
        assert_eq!(db.cmd_get("hello").unwrap(), None);
    }

    #[test]
    fn quotes_roundtrip() {
        let db = Database::open_in_memory().unwrap();
        let id = db.quote_add("hello world", "alice", 123456).unwrap();
        assert_eq!(id, 1);

        let q = db.quote_get(1).unwrap().unwrap();
        assert_eq!(q.1, "hello world");
        assert_eq!(q.2, "alice");

        assert_eq!(db.quote_count().unwrap(), 1);
        db.quote_remove(1).unwrap();
        assert_eq!(db.quote_count().unwrap(), 0);
    }

    #[test]
    fn counters_increment() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.counter_get("foo").unwrap(), 0);
        assert_eq!(db.counter_increment("foo").unwrap(), 1);
        assert_eq!(db.counter_increment("foo").unwrap(), 2);
        assert_eq!(db.counter_get("foo").unwrap(), 2);
    }

    #[test]
    fn reminders_due() {
        let db = Database::open_in_memory().unwrap();
        db.reminder_add("ch1", "u1", "alice", "check oven", 100).unwrap();
        db.reminder_add("ch1", "u2", "bob", "take break", 200).unwrap();
        db.reminder_add("ch2", "u3", "carol", "later", 300).unwrap();

        let due = db.reminder_due(150).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].4, "check oven");

        db.reminder_delete_due(150).unwrap();
        assert_eq!(db.reminder_count().unwrap(), 2);
    }
}
