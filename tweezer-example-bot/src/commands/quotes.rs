use std::time::Duration;

use tweezer::{Bot, Command, Context, ParseArgsError, RateLimitStrategy};

use crate::variables::QuotesDb;
use crate::Moderators;

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

pub fn add_commands(bot: &mut Bot) {
    bot.add_command(
        Command::new("addquote", |ctx: Context| async move {
            let text = ctx.args().join(" ");
            if text.is_empty() {
                return ctx.reply("usage: !addquote <text>").await;
            }
            let author = ctx.user().display().to_string();
            let db = match ctx.state::<QuotesDb>() {
                Some(d) => d,
                None => return ctx.reply("quotes db not available").await,
            };
            let id = db.add(&text, &author);
            ctx.reply(&format!("quote #{id} added")).await
        })
        .description("add a quote to the database")
        .category("general")
        .rate_limit(5, Duration::from_secs(60))
        .user_rate_limit(1, Duration::from_secs(30))
        .rate_limit_strategy(RateLimitStrategy::FixedWindow)
        .on_rate_limit(|ctx| async move {
            ctx.reply("adding quotes too fast, slow down").await
        }),
    );

    bot.add_command(
        Command::new("quote", |ctx: Context| async move {
            let db = match ctx.state::<QuotesDb>() {
                Some(d) => d,
                None => return ctx.reply("quotes db not available").await,
            };
            if db.count() == 0 {
                return ctx.reply("no quotes yet").await;
            }

            let quote = match ctx.parse_args::<(u32,)>() {
                Ok((id,)) => db.get(id as usize),
                Err(_) => db.random(),
            };

            match quote {
                Some(q) => {
                    let time = format_timestamp(q.timestamp);
                    ctx.reply(&format!(
                        "#{} \"{}\" — {} ({time})",
                        q.id, q.text, q.author
                    ))
                    .await
                }
                None => ctx.reply("quote not found").await,
            }
        })
        .description("show a random quote or specific one by id (e.g. !quote 5)")
        .category("general")
        .rate_limit(10, Duration::from_secs(60))
        .user_rate_limit(2, Duration::from_secs(30))
        .rate_limit_strategy(RateLimitStrategy::FixedWindow)
        .on_rate_limit(|ctx| async move {
            ctx.reply("quote lookups rate limited, slow down").await
        }),
    );

    bot.add_command(
        Command::new("delquote", |ctx: Context| async move {
            let (id,): (u32,) = match ctx.parse_args() {
                Ok(v) => v,
                Err(ParseArgsError::MissingArgument(_)) => {
                    return ctx.reply("usage: !delquote <id>").await;
                }
                Err(e) => {
                    return ctx.reply(&format!("bad args: {e}")).await;
                }
            };

            let db = match ctx.state::<QuotesDb>() {
                Some(d) => d,
                None => return ctx.reply("quotes db not available").await,
            };

            if db.remove(id as usize) {
                ctx.reply(&format!("quote #{id} removed")).await
            } else {
                ctx.reply(&format!("quote #{id} not found")).await
            }
        })
        .description("remove a quote by id (mod only)")
        .category("moderation")
        .check(|ctx| async move {
            ctx.state::<Moderators>()
                .map(|m| m.contains(ctx.user().name.as_str()))
                .unwrap_or(false)
        })
        .on_check_fail(|ctx| async move {
            ctx.reply("only the streamer can remove quotes").await
        }),
    );

    bot.add_command(
        Command::new("quotecount", |ctx: Context| async move {
            let db = match ctx.state::<QuotesDb>() {
                Some(d) => d,
                None => return ctx.reply("quotes db not available").await,
            };
            ctx.reply(&format!("{} quotes in the database", db.count())).await
        })
        .description("count quotes in the database")
        .category("general"),
    );
}

fn format_timestamp(ts: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let diff = now.saturating_sub(ts);
    if diff < 60 {
        "just now".to_string()
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else if diff < 604800 {
        format!("{}d ago", diff / 86400)
    } else {
        format!("{}w ago", diff / 604800)
    }
}
