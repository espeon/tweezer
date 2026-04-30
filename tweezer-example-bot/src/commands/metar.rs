use std::time::Duration;

use tweezer::{Bot, Command, Context, ParseArgsError, RateLimitStrategy, TweezerError};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

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
    match f {
        x if x < 20.0 => "Dangerously cold",
        x if x < 35.0 => "Bitterly cold",
        x if x < 50.0 => "Pretty chilly",
        x if x < 60.0 => "On the cool side",
        x if x < 70.0 => "Comfortable",
        x if x < 80.0 => "Nice and warm",
        x if x < 90.0 => "Getting pretty warm",
        x if x < 100.0 => "Straight up hot",
        _ => "Dangerously hot",
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

// ---------------------------------------------------------------------------
// API
// ---------------------------------------------------------------------------

async fn fetch_metar(icao: &str) -> Result<MetarResponse, TweezerError> {
    let url = format!("https://aviationweather.gov/api/data/metar?ids={icao}&format=json");
    let client = reqwest::Client::new();
    let resp: Vec<MetarResponse> = client
        .get(&url)
        .send()
        .await
        .map_err(|e| TweezerError::Connection(e.to_string()))?
        .json()
        .await
        .map_err(|e| TweezerError::Connection(e.to_string()))?;

    match resp.into_iter().next() {
        Some(m) => Ok(m),
        None => Err(TweezerError::Trigger(format!(
            "no METAR found for {icao}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Command
// ---------------------------------------------------------------------------

pub fn add_command(bot: &mut Bot) {
    bot.add_command(
        Command::new("metar", |ctx: Context| async move {
            let (icao, plain): (String, Option<String>) = match ctx.parse_args() {
                Ok(v) => v,
                Err(ParseArgsError::MissingArgument(_)) => {
                    return ctx.reply("usage: !metar <icao> [plain]").await;
                }
                Err(e) => {
                    return ctx.reply(&format!("bad args: {e}")).await;
                }
            };
            let icao = icao.to_uppercase();
            if !icao.chars().all(|c| c.is_ascii_alphabetic()) || icao.len() != 4 {
                return ctx.reply("ICAO codes are 4 letters, e.g. KMCI").await;
            }
            match fetch_metar(&icao).await {
                Ok(metar) => {
                    let text = if plain.as_deref() == Some("plain") {
                        metar.plain()
                    } else {
                        metar.raw_ob
                    };
                    ctx.reply(&text).await
                }
                Err(e) => ctx.reply(&format!("error: {e}")).await,
            }
        })
        .description("fetch current METAR for an airport (e.g. !metar KMCI or !metar KLAX plain)")
        .category("aviation")
        .rate_limit(10, Duration::from_secs(60))
        .user_rate_limit(1, Duration::from_secs(60))
        .rate_limit_strategy(RateLimitStrategy::FixedWindow)
        .on_rate_limit(|ctx| async move {
            let _ = ctx.delete().await;
            ctx.reply("metar rate limited, try again later").await
        }),
    );
}
