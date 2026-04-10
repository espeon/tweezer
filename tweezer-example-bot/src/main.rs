mod variables;

use std::collections::HashSet;

use tracing::info;
use tweezer::{Bot, Command, Context};
use tweezer_streamplace::StreamplaceAdapter;

use variables::{DynamicCommands, Expander, FileStore};

#[derive(serde::Deserialize)]
struct Cloud {
    cover: String,
}

#[derive(serde::Deserialize)]
struct MetarResponse {
    #[serde(rename = "rawOb")]
    raw_ob: String,
    name: Option<String>,
    temp: Option<f64>,
    dewp: Option<f64>,
    wdir: Option<i32>,
    wspd: Option<i32>,
    wgust: Option<i32>,
    visib: Option<String>,
    clouds: Option<Vec<Cloud>>,
}

impl MetarResponse {
    fn plain(&self) -> String {
        let mut s = String::new();

        if let Some(ref name) = self.name {
            s.push_str(name);
            s.push_str(": ");
        }

        if let Some(t) = self.temp {
            let f = t * 9.0 / 5.0 + 32.0;
            let temp_desc = describe_temp(f);
            s.push_str(&format!("{temp_desc} ({:.0}°F)", f));
            if let Some(d) = self.dewp {
                let df = d * 9.0 / 5.0 + 32.0;
                let spread = f - df;
                if spread < 5.0 {
                    s.push_str(", pretty humid out");
                } else if spread > 30.0 {
                    s.push_str(", air is real dry though");
                }
            }
            s.push_str(". ");
        }

        if let (Some(dir), Some(spd)) = (self.wdir, self.wspd) {
            if spd == 0 {
                s.push_str("Not much wind right now. ");
            } else {
                let cardinal = dir_to_cardinal(dir);
                s.push_str(&format!("Wind coming out of the {cardinal} at {spd} kt"));
                if let Some(gust) = self.wgust {
                    if gust > 0 {
                        s.push_str(&format!(", gusting to {gust}"));
                    }
                }
                s.push('.');
                if spd >= 25 || self.wgust.map_or(false, |g| g >= 35) {
                    s.push_str(" Hold onto your hat.");
                } else if spd >= 15 {
                    s.push_str(" Bit of a breeze.");
                }
                s.push(' ');
            }
        }

        if let Some(ref vis) = self.visib {
            if vis.contains("10") || vis.contains("9999") {
                s.push_str("Visibility is good. ");
            } else if vis.starts_with('<') || vis.starts_with('0') {
                s.push_str("Pretty socked in out there. ");
            } else {
                s.push_str(&format!("Visibility around {vis} statute miles. "));
            }
        }

        if let Some(ref layers) = self.clouds {
            if layers.is_empty() {
                s.push_str("Clear skies.");
            } else {
                let sky: &str = match layers.last().map(|c| c.cover.as_str()).unwrap_or("") {
                    "SKC" | "CLR" => "Clear skies.",
                    "FEW" => "Just a few clouds hanging around.",
                    "SCT" => "Broken up clouds, some sun getting through.",
                    "BKN" => "Pretty overcast.",
                    "OVC" => "Full grey blanket overhead.",
                    _ => "Some cloud cover up there.",
                };
                s.push_str(sky);
            }
        }

        let trimmed = s.trim();
        if trimmed.is_empty() {
            self.raw_ob.clone()
        } else {
            trimmed.to_string()
        }
    }
}

fn describe_temp(f: f64) -> &'static str {
    if f < 20.0 {
        "Dangerously cold"
    } else if f < 35.0 {
        "Bitterly cold"
    } else if f < 50.0 {
        "Pretty chilly"
    } else if f < 60.0 {
        "On the cool side"
    } else if f < 70.0 {
        "Comfortable"
    } else if f < 80.0 {
        "Nice and warm"
    } else if f < 90.0 {
        "Getting pretty warm"
    } else if f < 100.0 {
        "Straight up hot"
    } else {
        "Dangerously hot"
    }
}

fn dir_to_cardinal(deg: i32) -> &'static str {
    match deg {
        338..=360 | 0..=22 => "north",
        23..=67 => "northeast",
        68..=112 => "east",
        113..=157 => "southeast",
        158..=202 => "south",
        203..=247 => "southwest",
        248..=292 => "west",
        293..=337 => "northwest",
        _ => "unknown",
    }
}

async fn fetch_metar(icao: &str) -> Result<MetarResponse, tweezer::TweezerError> {
    let url = format!("https://aviationweather.gov/api/data/metar?ids={icao}&format=json");
    let client = reqwest::Client::new();
    let resp: Vec<MetarResponse> = client
        .get(&url)
        .send()
        .await
        .map_err(|e| tweezer::TweezerError::Connection(e.to_string()))?
        .json()
        .await
        .map_err(|e| tweezer::TweezerError::Connection(e.to_string()))?;

    match resp.into_iter().next() {
        Some(m) => Ok(m),
        None => Err(tweezer::TweezerError::Trigger(format!(
            "no METAR found for {icao}"
        ))),
    }
}

/// User IDs allowed to add/remove dynamic commands.
pub struct Moderators(pub HashSet<String>);

impl Moderators {
    pub fn contains(&self, id: &str) -> bool {
        self.0.contains(id)
    }
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tweezer=info".parse().unwrap()),
        )
        .init();

    let jetstream_url = std::env::var("JETSTREAM_URL")
        .unwrap_or_else(|_| "wss://nyc.firehose.stream/subscribe".into());
    let identifier = std::env::var("BOT_IDENTIFIER").expect("BOT_IDENTIFIER is required");
    let password = std::env::var("BOT_PASSWORD").expect("BOT_PASSWORD is required");

    let streamers =
        std::env::var("STREAMER_DIDS").expect("STREAMER_DIDS is required (comma-separated)");
    let streamer_dids: Vec<String> = streamers.split(',').map(|s| s.trim().to_string()).collect();

    let mut adapter = StreamplaceAdapter::new(jetstream_url, identifier, password);
    adapter.add_streamers(streamer_dids.clone());

    let mut bot = Bot::new();

    bot.register(
        DynamicCommands::new(std::sync::Arc::new(Expander::new()))
            .with_store(FileStore::new("commands.json")),
    );
    bot.register(Moderators(streamer_dids.into_iter().collect()));

    // ---------------------------------------------------------------------------
    // Static commands
    // ---------------------------------------------------------------------------

    bot.add_command(
        Command::new(
            "ping",
            |ctx: Context| async move { ctx.reply("pong").await },
        )
        .description("responds with pong")
        .category("general"),
    );

    bot.add_command(
        Command::new("lurk", |ctx: Context| {
            let name = ctx.user().display().to_string();
            async move {
                info!("{name} is lurking");
                ctx.reply(&format!("@{name} is now lurking")).await
            }
        })
        .description("let everyone know you're going quiet")
        .category("general"),
    );

    bot.add_command(
        Command::new("wat", |ctx: Context| {
            let name = ctx.user().display().to_string();
            async move { ctx.reply(&format!("@{name} z.ai https://github.com")).await }
        })
        .description("wat")
        .category("general"),
    );

    bot.add_command(
        Command::new("metar", |ctx: Context| {
            let icao = ctx.args().first().cloned();
            let plain = ctx.args().get(1).map(|s| s.as_str()) == Some("plain");
            async move {
                let icao = match icao {
                    Some(code) => code.to_uppercase(),
                    None => {
                        return ctx
                            .reply("usage: !metar <icao> [plain]  (e.g. !metar KMCI)")
                            .await;
                    }
                };
                if !icao.chars().all(|c| c.is_ascii_alphabetic()) || icao.len() != 4 {
                    return ctx.reply("ICAO codes are 4 letters, e.g. KMCI").await;
                }
                match fetch_metar(&icao).await {
                    Ok(metar) => {
                        let text = if plain { metar.plain() } else { metar.raw_ob };
                        ctx.reply(&text).await
                    }
                    Err(e) => ctx.reply(&format!("error: {e}")).await,
                }
            }
        })
        .description("fetch current METAR for an airport (e.g. !metar KMCI or !metar KLAX plain)")
        .category("aviation"),
    );

    bot.add_command(
        Command::new("timer", |ctx: Context| async move {
            ctx.reply(
                "The timer counts down to Unix time 173173173173.173. \
                 173 on an upside-down calculator spells Eli.",
            )
            .await
        })
        .description("what is the timer?")
        .category("streamplace")
        .channel("did:web:stream.place"),
    );

    // ---------------------------------------------------------------------------
    // Dynamic command management
    // ---------------------------------------------------------------------------

    bot.add_command(
        Command::new("addcmd", |ctx: Context| async move {
            let mods = ctx
                .state::<Moderators>()
                .expect("Moderators not registered");
            if !mods.contains(ctx.user().name.as_str()) {
                return ctx.reply("only the streamer can add commands").await;
            }
            let dc = ctx
                .state::<DynamicCommands>()
                .expect("DynamicCommands not registered");
            let name = match ctx.args().first() {
                Some(n) => n.trim_start_matches('!').to_string(),
                None => return ctx.reply("usage: !addcmd !name template text").await,
            };
            if ctx.args().len() < 2 {
                return ctx.reply("usage: !addcmd !name template text").await;
            }
            let template = ctx.args()[1..].join(" ");
            dc.add(&name, &template);
            ctx.reply(&format!("command !{name} added")).await
        })
        .description("add a dynamic command: !addcmd !name template")
        .category("moderation"),
    );

    bot.add_command(
        Command::new("delcmd", |ctx: Context| async move {
            let mods = ctx
                .state::<Moderators>()
                .expect("Moderators not registered");
            if !mods.contains(ctx.user().name.as_str()) {
                return ctx.reply("only the streamer can remove commands").await;
            }
            let dc = ctx
                .state::<DynamicCommands>()
                .expect("DynamicCommands not registered");
            let name = match ctx.args().first() {
                Some(n) => n.trim_start_matches('!').to_string(),
                None => return ctx.reply("usage: !delcmd !name").await,
            };
            if dc.remove(&name) {
                ctx.reply(&format!("command !{name} removed")).await
            } else {
                ctx.reply(&format!("no command named !{name}")).await
            }
        })
        .description("remove a dynamic command: !delcmd !name")
        .category("moderation"),
    );

    bot.add_command(
        Command::new("cmds", |ctx: Context| async move {
            let dc = ctx
                .state::<DynamicCommands>()
                .expect("DynamicCommands not registered");
            let names = dc.list();
            if names.is_empty() {
                ctx.reply("no custom commands yet").await
            } else {
                ctx.reply(&format!("custom commands: !{}", names.join(", !")))
                    .await
            }
        })
        .description("list dynamic commands")
        .category("general"),
    );

    bot.help_command();

    // ---------------------------------------------------------------------------
    // Dynamic command dispatch (on_message catch-all)
    // ---------------------------------------------------------------------------

    bot.on_message(|ctx: Context| async move {
        let dc = ctx
            .state::<DynamicCommands>()
            .expect("DynamicCommands not registered");
        if let Some(reply) = dc.try_dispatch(&ctx.message, &ctx).await {
            ctx.reply(&reply).await?;
        }
        Ok(())
    });

    bot.on_error(|e| {
        tracing::error!(error = %e.error, "handler error");
    });

    bot.add_adapter(adapter);
    bot.run().await.expect("bot exited with error");
}
