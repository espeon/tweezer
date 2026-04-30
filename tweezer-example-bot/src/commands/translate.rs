use std::time::Duration;

use tlx_client::translator::TLClient;
use tweezer::{Bot, Command, Context, RateLimitStrategy};

// ---------------------------------------------------------------------------
// Translation command
// ---------------------------------------------------------------------------

pub fn add_command(bot: &mut Bot) {
    bot.add_command(
        Command::new("translate", |ctx: Context| async move {
            let args = ctx.args();
            if args.len() < 2 {
                return ctx
                    .reply("usage: !translate <to_lang> <text>  or  !translate <from_lang> <to_lang> <text>")
                    .await;
            }

            let (from, to, text) = if args.len() == 2 {
                // !translate en hello
                (None, args[0].clone(), args[1].clone())
            } else {
                // !translate auto en hello world
                let f = if args[0].eq_ignore_ascii_case("auto") {
                    None
                } else {
                    Some(args[0].clone())
                };
                (f, args[1].clone(), args[2..].join(" "))
            };

            if text.is_empty() {
                return ctx.reply("need some text to translate").await;
            }

            let result = tokio::task::spawn_blocking(move || {
                let client = tlx_client::translator::google::GoogleTranslate::new();
                client.translate(&text, from, &to)
            })
            .await;

            match result {
                Ok(Ok(resp)) => {
                    ctx.reply(&format!(
                        "[{} → {}] {}",
                        resp.source_lang.to_lowercase(),
                        resp.target_lang.to_lowercase(),
                        resp.data
                    ))
                    .await
                }
                Ok(Err(e)) => ctx.reply(&format!("translation error: {e}")).await,
                Err(e) => ctx.reply(&format!("translation failed: {e}")).await,
            }
        })
        .description("translate text (e.g. !translate ja hello world, !translate auto de good morning)")
        .category("general")
        .rate_limit(10, Duration::from_secs(60))
        .user_rate_limit(2, Duration::from_secs(60))
        .rate_limit_strategy(RateLimitStrategy::FixedWindow)
        .on_rate_limit(|ctx| async move {
            ctx.reply("translation rate limited, slow down").await
        }),
    );
}
