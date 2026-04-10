#[derive(Debug, Clone)]
pub struct User {
    /// Stable platform identifier: DID on Streamplace, login name on Twitch, etc.
    pub name: String,
    pub id: String,
    /// Human-readable display name or handle, if resolved by the adapter.
    pub display_name: Option<String>,
}

impl User {
    /// Returns the display name if set, otherwise falls back to `name`.
    pub fn display(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.name)
    }
}
