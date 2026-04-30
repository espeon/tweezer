use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::sync::RwLock;
use tokio::task::JoinSet;
use tweezer::{Bot, Command, Context, RateLimitStrategy, TweezerError};

// ---------------------------------------------------------------------------
// PokeAPI types
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct PokeTypeSlot {
    #[serde(rename = "type")]
    type_: PokeType,
}

#[derive(serde::Deserialize, Clone)]
struct PokeType {
    name: String,
}

#[derive(serde::Deserialize)]
struct PokemonResponse {
    id: u32,
    name: String,
    types: Vec<PokeTypeSlot>,
    height: i32,
    weight: i32,
}

impl PokemonResponse {
    fn summary(&self) -> String {
        let types: Vec<String> = self
            .types
            .iter()
            .map(|t| {
                let name = &t.type_.name;
                name[..1].to_uppercase() + &name[1..]
            })
            .collect();
        let type_str = if types.len() > 1 {
            format!("{} type", types.join("/"))
        } else {
            format!("{} type", types[0])
        };
        let height_m = self.height as f64 / 10.0;
        let weight_kg = self.weight as f64 / 10.0;
        format!(
            "#{} {}: {}. {:.1}m, {:.1}kg.",
            self.id,
            self.name[..1].to_uppercase() + &self.name[1..],
            type_str,
            height_m,
            weight_kg
        )
    }
}

#[derive(serde::Deserialize)]
struct PokemonListEntry {
    name: String,
    url: String,
}

#[derive(serde::Deserialize)]
struct PokemonListResponse {
    results: Vec<PokemonListEntry>,
}

// ---------------------------------------------------------------------------
// Type chart
// ---------------------------------------------------------------------------

const ALL_TYPES: &[&str] = &[
    "normal", "fire", "water", "electric", "grass", "ice", "fighting", "poison", "ground",
    "flying", "psychic", "bug", "rock", "ghost", "dragon", "dark", "steel", "fairy",
];

#[derive(serde::Deserialize)]
struct TypeRelations {
    #[serde(rename = "double_damage_from")]
    double: Vec<PokeType>,
    #[serde(rename = "half_damage_from")]
    half: Vec<PokeType>,
    #[serde(rename = "no_damage_from")]
    none: Vec<PokeType>,
}

#[derive(serde::Deserialize)]
struct TypeResponse {
    #[serde(rename = "damage_relations")]
    relations: TypeRelations,
}

#[derive(Clone)]
pub struct TypeChart {
    chart: HashMap<String, HashMap<String, f64>>,
}

impl TypeChart {
    fn from_chart(chart: HashMap<String, HashMap<String, f64>>) -> Self {
        Self { chart }
    }

    async fn load() -> Result<Self, TweezerError> {
        let client = reqwest::Client::new();
        let mut set = JoinSet::new();

        for name in ALL_TYPES {
            let client = client.clone();
            let name = name.to_string();
            set.spawn(async move {
                let resp: TypeResponse = client
                    .get(&format!("https://pokeapi.co/api/v2/type/{name}"))
                    .send()
                    .await
                    .map_err(|e| TweezerError::Connection(e.to_string()))?
                    .json()
                    .await
                    .map_err(|e| TweezerError::Connection(e.to_string()))?;
                Ok::<(String, TypeResponse), TweezerError>((name, resp))
            });
        }

        let mut chart = HashMap::new();
        while let Some(result) = set.join_next().await {
            let (name, resp) = result.map_err(|e| TweezerError::Connection(e.to_string()))??;
            let mut multipliers = HashMap::new();
            for t in &resp.relations.double {
                multipliers.insert(t.name.clone(), 2.0);
            }
            for t in &resp.relations.half {
                multipliers.insert(t.name.clone(), 0.5);
            }
            for t in &resp.relations.none {
                multipliers.insert(t.name.clone(), 0.0);
            }
            chart.insert(name, multipliers);
        }

        Ok(Self { chart })
    }

    fn analyze(&self, types: &[String]) -> String {
        let mut quad: Vec<String> = Vec::new();
        let mut weak: Vec<String> = Vec::new();
        let mut resist: Vec<String> = Vec::new();
        let mut quarter: Vec<String> = Vec::new();
        let mut immune: Vec<String> = Vec::new();

        for attacker in ALL_TYPES {
            let mut mult = 1.0;
            for defender in types {
                if let Some(def_chart) = self.chart.get(defender) {
                    mult *= def_chart.get(*attacker).copied().unwrap_or(1.0);
                }
            }
            let display = attacker[..1].to_uppercase() + &attacker[1..];
            match mult {
                4.0 => quad.push(display),
                2.0 => weak.push(display),
                0.5 => resist.push(display),
                0.25 => quarter.push(display),
                0.0 => immune.push(display),
                _ => {}
            }
        }

        let mut parts: Vec<String> = Vec::new();
        if !quad.is_empty() {
            parts.push(format!("4x: {}", quad.join(", ")));
        }
        if !weak.is_empty() {
            parts.push(format!("weak: {}", weak.join(", ")));
        }
        if !resist.is_empty() {
            parts.push(format!("resist: {}", resist.join(", ")));
        }
        if !quarter.is_empty() {
            parts.push(format!("0.25x: {}", quarter.join(", ")));
        }
        if !immune.is_empty() {
            parts.push(format!("immune: {}", immune.join(", ")));
        }

        if parts.is_empty() {
            "no notable type interactions".into()
        } else {
            parts.join(" | ")
        }
    }
}

#[derive(Clone)]
pub struct TypeChartCache {
    chart: Arc<RwLock<Option<TypeChart>>>,
}

impl TypeChartCache {
    pub fn new() -> Self {
        Self {
            chart: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn get(&self) -> Result<TypeChart, TweezerError> {
        {
            let read = self.chart.read().await;
            if let Some(ref chart) = *read {
                return Ok(chart.clone());
            }
        }

        let chart = TypeChart::load().await?;
        let mut write = self.chart.write().await;
        *write = Some(chart.clone());
        Ok(chart)
    }
}

// ---------------------------------------------------------------------------
// Search cache
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct PokemonCache {
    entries: Arc<RwLock<Vec<(String, u32)>>>,
}

impl PokemonCache {
    pub fn new() -> Self {
        Self {
            entries: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub async fn ensure_loaded(&self) -> Result<(), TweezerError> {
        let read = self.entries.read().await;
        if !read.is_empty() {
            return Ok(());
        }
        drop(read);

        let mut write = self.entries.write().await;
        if !write.is_empty() {
            return Ok(());
        }

        let client = reqwest::Client::new();
        let resp: PokemonListResponse = client
            .get("https://pokeapi.co/api/v2/pokemon?limit=100000")
            .send()
            .await
            .map_err(|e| TweezerError::Connection(e.to_string()))?
            .json()
            .await
            .map_err(|e| TweezerError::Connection(e.to_string()))?;

        let entries: Vec<(String, u32)> = resp
            .results
            .into_iter()
            .filter_map(|r| {
                let id = r
                    .url
                    .trim_end_matches('/')
                    .split('/')
                    .last()?
                    .parse()
                    .ok()?;
                Some((r.name, id))
            })
            .collect();

        *write = entries;
        Ok(())
    }

    pub async fn search(&self, query: &str) -> Vec<(String, u32)> {
        if self.ensure_loaded().await.is_err() {
            return Vec::new();
        }
        let query = query.to_lowercase();
        let entries = self.entries.read().await;
        entries
            .iter()
            .filter(|(name, _)| name.contains(&query))
            .take(5)
            .cloned()
            .collect()
    }
}

// ---------------------------------------------------------------------------
// API helpers
// ---------------------------------------------------------------------------

async fn fetch_pokemon(name: &str) -> Result<PokemonResponse, TweezerError> {
    let url = format!("https://pokeapi.co/api/v2/pokemon/{name}");
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| TweezerError::Connection(e.to_string()))?;

    if resp.status() == 404 {
        return Err(TweezerError::Trigger(format!("no pokemon named '{name}'")));
    }

    resp.json()
        .await
        .map_err(|e| TweezerError::Connection(e.to_string()))
}

// ---------------------------------------------------------------------------
// Command registration
// ---------------------------------------------------------------------------

pub fn add_command(bot: &mut Bot) {
    bot.add_command(
        Command::new("pokemon", |ctx: Context| async move {
            let args = ctx.args();
            if args.is_empty() {
                return ctx.reply("usage: !pokemon <name or #>  or  !pokemon weakness <type1> [type2]").await;
            }

            // -----------------------------------------------------------------
            // weakness subcommand
            // -----------------------------------------------------------------
            if args[0] == "weakness" {
                let types: Vec<String> = args[1..]
                    .iter()
                    .map(|s| s.to_lowercase())
                    .filter(|s| !s.is_empty())
                    .collect();

                if types.is_empty() {
                    return ctx.reply("usage: !pokemon weakness <type1> [type2]").await;
                }
                if types.len() > 2 {
                    return ctx.reply("max 2 types please").await;
                }

                for t in &types {
                    if !ALL_TYPES.contains(&t.as_str()) {
                        return ctx.reply(&format!("'{t}' isn't a valid type. valid types: {}", ALL_TYPES.join(", "))).await;
                    }
                }

                let cache = match ctx.state::<TypeChartCache>() {
                    Some(c) => c,
                    None => return ctx.reply("type chart not available").await,
                };

                let chart = match cache.get().await {
                    Ok(c) => c,
                    Err(_) => return ctx.reply("couldn't load type chart from pokeapi").await,
                };

                let type_names: Vec<String> = types.iter().map(|t| {
                    t[..1].to_uppercase() + &t[1..]
                }).collect();
                let header = if type_names.len() > 1 {
                    format!("{} / {}", type_names[0], type_names[1])
                } else {
                    type_names[0].clone()
                };

                return ctx.reply(&format!("{header}: {}", chart.analyze(&types))).await;
            }

            // -----------------------------------------------------------------
            // pokemon lookup
            // -----------------------------------------------------------------
            let name = args.join(" ");

            // Try exact lookup first
            match fetch_pokemon(&name).await {
                Ok(mon) => return ctx.reply(&mon.summary()).await,
                Err(TweezerError::Connection(_)) => {
                    return ctx.reply("pokeapi is having issues, try again later").await;
                }
                Err(TweezerError::Trigger(_)) => {
                    // Not found — fall through to search
                }
                Err(e) => return ctx.reply(&format!("error: {e}")).await,
            }

            // Search the cache for suggestions
            if let Some(cache) = ctx.state::<PokemonCache>() {
                let matches = cache.search(&name).await;
                if !matches.is_empty() {
                    let suggestions: Vec<String> = matches
                        .into_iter()
                        .map(|(n, id)| format!("#{id} {n}"))
                        .collect();
                    return ctx
                        .reply(&format!(
                            "no pokemon '{name}'. did you mean: {}",
                            suggestions.join(", ")
                        ))
                        .await;
                }
            }

            ctx.reply(&format!("no pokemon named '{name}'")).await
        })
        .description("look up a pokemon or type weaknesses (e.g. !pokemon charizard, !pokemon weakness grass poison)")
        .category("games")
        .rate_limit(10, Duration::from_secs(60))
        .user_rate_limit(1, Duration::from_secs(60))
        .rate_limit_strategy(RateLimitStrategy::FixedWindow)
        .on_rate_limit(|ctx| async move {
            ctx.reply("pokemon lookups rate limited, try again later").await
        }),
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_chart_single_type() {
        let mut chart = HashMap::new();
        let mut fire = HashMap::new();
        fire.insert("water".to_string(), 2.0);
        fire.insert("rock".to_string(), 2.0);
        fire.insert("fire".to_string(), 0.5);
        chart.insert("fire".to_string(), fire);

        let tc = TypeChart::from_chart(chart);
        let out = tc.analyze(&["fire".to_string()]);

        assert!(out.contains("weak: Water, Rock"), "{out}");
        assert!(out.contains("resist: Fire"), "{out}");
    }

    #[test]
    fn type_chart_dual_type() {
        let mut chart = HashMap::new();
        let mut grass = HashMap::new();
        grass.insert("fire".to_string(), 2.0);
        grass.insert("water".to_string(), 0.5);
        grass.insert("grass".to_string(), 0.5);
        chart.insert("grass".to_string(), grass);

        let mut poison = HashMap::new();
        poison.insert("ground".to_string(), 2.0);
        poison.insert("grass".to_string(), 0.5);
        chart.insert("poison".to_string(), poison);

        let tc = TypeChart::from_chart(chart);
        let out = tc.analyze(&["grass".to_string(), "poison".to_string()]);

        // fire(2) * poison-neutral(1) = 2x weak
        assert!(out.contains("weak: Fire"), "{out}");
        // ground(1) * grass-neutral(1) = 2x weak
        assert!(out.contains("Ground"), "{out}");
        // water(0.5) * poison-neutral(1) = 0.5x resist
        assert!(out.contains("resist: Water"), "{out}");
        // grass(0.5) * poison(0.5) = 0.25x double resist
        assert!(out.contains("0.25x: Grass"), "{out}");
    }

    #[test]
    fn type_chart_immunity_cancels() {
        let mut chart = HashMap::new();
        let mut ground = HashMap::new();
        ground.insert("water".to_string(), 2.0);
        ground.insert("electric".to_string(), 0.0);
        chart.insert("ground".to_string(), ground);

        let mut flying = HashMap::new();
        flying.insert("electric".to_string(), 2.0);
        flying.insert("ground".to_string(), 0.0);
        chart.insert("flying".to_string(), flying);

        let tc = TypeChart::from_chart(chart);
        let out = tc.analyze(&["ground".to_string(), "flying".to_string()]);

        // electric: ground immune(0) * flying weak(2) = 0 (immune takes precedence)
        assert!(out.contains("immune: Electric"), "{out}");
        // ground: ground neutral(1) * flying immune(0) = 0
        assert!(
            out.contains("immune: Ground") || out.contains(", Ground"),
            "{out}"
        );
    }
}
