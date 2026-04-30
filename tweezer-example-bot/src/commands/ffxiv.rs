use std::time::Duration;

use tweezer::{Bot, Command, Context, RateLimitStrategy, TweezerError};

const XIVAPI_V2: &str = "https://v2.xivapi.com/api";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct XivSearchResult {
    results: Vec<XivSearchRow>,
}

#[derive(serde::Deserialize)]
struct XivSearchRow {
    row_id: u32,
}

#[derive(serde::Deserialize)]
struct XivItemDetail {
    fields: XivItemFields,
}

#[derive(serde::Deserialize)]
struct XivItemFields {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "LevelItem")]
    level_item: XivLevelItem,
    #[serde(rename = "ClassJobCategory")]
    class_job_category: XivJobCategory,
    #[serde(rename = "Description")]
    description: Option<String>,
}

#[derive(serde::Deserialize)]
struct XivLevelItem {
    value: u32,
}

#[derive(serde::Deserialize)]
struct XivJobCategory {
    fields: XivJobCategoryFields,
}

#[derive(serde::Deserialize)]
struct XivJobCategoryFields {
    #[serde(rename = "Name")]
    name: String,
}

impl XivItemDetail {
    fn summary(&self) -> String {
        let f = &self.fields;
        let mut s = format!("{}: iLv {}", f.name, f.level_item.value);
        let job_name = &f.class_job_category.fields.name;
        if !job_name.is_empty() && job_name != "All Classes" {
            s.push_str(&format!(", {}", job_name));
        }
        if let Some(ref desc) = f.description {
            let plain = desc.replace("<br>", " ").replace('\n', " ");
            let trimmed = plain.trim();
            if !trimmed.is_empty() {
                s.push_str(&format!(". {}", trimmed));
            }
        }
        s
    }
}

// ---------------------------------------------------------------------------
// API
// ---------------------------------------------------------------------------

async fn fetch_ffxiv_item(name: &str) -> Result<XivItemDetail, TweezerError> {
    let client = reqwest::Client::new();
    let search_url = format!(
        "{}/search?sheets=Item&query=Name~%22{}%22&limit=1&fields=Name",
        XIVAPI_V2,
        urlencoding::encode(name)
    );
    let search: XivSearchResult = client
        .get(&search_url)
        .send()
        .await
        .map_err(|e| TweezerError::Connection(e.to_string()))?
        .json()
        .await
        .map_err(|e| TweezerError::Connection(e.to_string()))?;

    let result = search
        .results
        .into_iter()
        .next()
        .ok_or_else(|| TweezerError::Trigger(format!("no item named '{name}'")))?;

    let detail_url = format!(
        "{}/sheet/Item/{}?fields=Name,LevelItem,ClassJobCategory.Name,Description",
        XIVAPI_V2, result.row_id
    );
    let detail: XivItemDetail = client
        .get(&detail_url)
        .send()
        .await
        .map_err(|e| TweezerError::Connection(e.to_string()))?
        .json()
        .await
        .map_err(|e| TweezerError::Connection(e.to_string()))?;

    Ok(detail)
}

// ---------------------------------------------------------------------------
// Command
// ---------------------------------------------------------------------------

pub fn add_command(bot: &mut Bot) {
    bot.add_command(
        Command::new("ffxiv", |ctx: Context| async move {
            let name = ctx.args().join(" ");
            if name.is_empty() {
                return ctx
                    .reply("usage: !ffxiv <item name>  (e.g. !ffxiv ironworks sword)")
                    .await;
            }
            match fetch_ffxiv_item(&name).await {
                Ok(item) => ctx.reply(&item.summary()).await,
                Err(e) => ctx.reply(&format!("error: {e}")).await,
            }
        })
        .description("look up a FFXIV item by name (e.g. !ffxiv ironworks)")
        .category("games")
        .rate_limit(10, Duration::from_secs(60))
        .user_rate_limit(1, Duration::from_secs(60))
        .rate_limit_strategy(RateLimitStrategy::FixedWindow)
        .on_rate_limit(|ctx| async move {
            ctx.reply("ffxiv lookups rate limited, try again later")
                .await
        }),
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "makes real HTTP requests"]
    async fn v2_api_lookup_ironworks() {
        let item = fetch_ffxiv_item("ironworks").await.unwrap();
        let summary = item.summary();
        assert!(summary.contains("Ironworks"), "{summary}");
        assert!(summary.contains("iLv"), "{summary}");
    }

    #[tokio::test]
    #[ignore = "makes real HTTP requests"]
    async fn v2_api_unknown_item() {
        let result = fetch_ffxiv_item("xyznotreal12345").await;
        assert!(result.is_err());
    }
}
