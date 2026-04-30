use std::time::Duration;

use tweezer::{Bot, Command, Context, RateLimitStrategy, TweezerError};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct GenshinCharacter {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    vision: String,
    weapon: String,
    nation: String,
    rarity: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

impl GenshinCharacter {
    fn summary(&self) -> String {
        let stars = "★".repeat(self.rarity as usize);
        let mut s = format!(
            "{} {}: {} {} user from {}.",
            self.name, stars, self.vision, self.weapon, self.nation
        );
        if let Some(ref desc) = self.description {
            let trimmed = desc.trim();
            if !trimmed.is_empty() {
                s.push(' ');
                s.push_str(trimmed);
            }
        }
        s
    }
}

// ---------------------------------------------------------------------------
// API
// ---------------------------------------------------------------------------

async fn fetch_genshin_character(name: &str) -> Result<GenshinCharacter, TweezerError> {
    let key = name.to_lowercase().replace(' ', "-");
    let url = format!("https://api.genshin.dev/characters/{key}");
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| TweezerError::Connection(e.to_string()))?;

    if resp.status() == 404 {
        return Err(TweezerError::Trigger(format!(
            "no character named '{name}'"
        )));
    }

    resp.json()
        .await
        .map_err(|e| TweezerError::Connection(e.to_string()))
}

// ---------------------------------------------------------------------------
// Command
// ---------------------------------------------------------------------------

pub fn add_command(bot: &mut Bot) {
    bot.add_command(
        Command::new("genshin", |ctx: Context| async move {
            let name = ctx.args().join(" ");
            if name.is_empty() {
                return ctx
                    .reply("usage: !genshin <character>  (e.g. !genshin raiden, !genshin hu tao)")
                    .await;
            }
            match fetch_genshin_character(&name).await {
                Ok(ch) => ctx.reply(&ch.summary()).await,
                Err(e) => ctx.reply(&format!("error: {e}")).await,
            }
        })
        .description("look up a Genshin character (e.g. !genshin raiden)")
        .category("games")
        .rate_limit(10, Duration::from_secs(60))
        .user_rate_limit(1, Duration::from_secs(60))
        .rate_limit_strategy(RateLimitStrategy::FixedWindow)
        .on_rate_limit(|ctx| async move {
            ctx.reply("genshin lookups rate limited, try again later")
                .await
        }),
    );
}
