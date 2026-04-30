#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BadgeKind {
    Streamer,
    Moderator,
    Bot,
    Vip,
    Event,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadgeInfo {
    pub name: String,
    pub kind: BadgeKind,
}

#[derive(Debug, Clone)]
pub struct User {
    /// Stable platform identifier: DID on Streamplace, login name on Twitch, etc.
    pub name: String,
    pub id: String,
    /// Human-readable display name or handle, if resolved by the adapter.
    pub display_name: Option<String>,
    /// Chat color from the user's profile, if set.
    pub color: Option<ChatColor>,
    /// Self-labels from the user's chat profile (e.g., "bot").
    pub labels: Vec<String>,
    /// Badges visible for this user in the current channel context.
    pub badges: Vec<BadgeInfo>,
}

impl User {
    /// Returns the display name if set, otherwise falls back to `name`.
    pub fn display(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.name)
    }

    /// True if the user has self-labeled as a bot.
    pub fn is_bot(&self) -> bool {
        self.labels.iter().any(|l| l.eq_ignore_ascii_case("bot"))
    }
}
