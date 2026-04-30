use std::time::Duration;

use tweezer::{Bot, Command, Context, RateLimitStrategy};

use crate::commands::polls::PollManager;
use crate::Moderators;

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

pub fn add_commands(bot: &mut Bot) {
    bot.add_command(
        Command::new("poll", |ctx: Context| async move {
            let args = ctx.args();
            if args.len() < 3 {
                return ctx
                    .reply("usage: !poll \"<question>\" option1 option2 [option3...]")
                    .await;
            }

            let question = args[0].clone();
            let options: Vec<String> = args[1..].to_vec();

            let mgr = match ctx.state::<PollManager>() {
                Some(m) => m,
                None => return ctx.reply("poll manager not available").await,
            };

            let channel = ctx.channel().to_string();
            match mgr.start(&channel, &question, options) {
                Ok(()) => {
                    let opt_list: Vec<String> = mgr
                        .get(&channel)
                        .unwrap()
                        .options
                        .iter()
                        .enumerate()
                        .map(|(i, o)| format!("{} {}", i + 1, o.label))
                        .collect();
                    ctx.reply(&format!(
                        "📊 {} — vote with !vote <number>: {}",
                        question,
                        opt_list.join(", ")
                    ))
                    .await
                }
                Err(e) => ctx.reply(&format!("couldn't start poll: {e}")).await,
            }
        })
        .description("start a poll (e.g. !poll \"best color?\" red blue green)")
        .category("general")
        .rate_limit(3, Duration::from_secs(300))
        .user_rate_limit(1, Duration::from_secs(300))
        .rate_limit_strategy(RateLimitStrategy::FixedWindow)
        .on_rate_limit(|ctx| async move {
            ctx.reply("poll creation rate limited, slow down").await
        }),
    );

    bot.add_command(
        Command::new("vote", |ctx: Context| async move {
            let args = ctx.args();
            if args.is_empty() {
                return ctx.reply("usage: !vote <option number>").await;
            }
            let choice: usize = match args[0].parse() {
                Ok(n) => n,
                Err(_) => return ctx.reply("vote with a number, e.g. !vote 1").await,
            };

            let mgr = match ctx.state::<PollManager>() {
                Some(m) => m,
                None => return ctx.reply("poll manager not available").await,
            };

            let channel = ctx.channel().to_string();
            let user_id = ctx.user().id.clone();
            match mgr.vote(&channel, &user_id, choice) {
                Ok(()) => ctx.reply("vote counted").await,
                Err(e) => ctx.reply(&format!("couldn't vote: {e}")).await,
            }
        })
        .description("vote in the active poll (e.g. !vote 1)")
        .category("general")
        .rate_limit(5, Duration::from_secs(60))
        .user_rate_limit(1, Duration::from_secs(10))
        .rate_limit_strategy(RateLimitStrategy::FixedWindow)
        .on_rate_limit(|ctx| async move {
            ctx.reply("voting too fast, slow down").await
        }),
    );

    bot.add_command(
        Command::new("endpoll", |ctx: Context| async move {
            let mgr = match ctx.state::<PollManager>() {
                Some(m) => m,
                None => return ctx.reply("poll manager not available").await,
            };

            let channel = ctx.channel().to_string();
            match mgr.end(&channel) {
                Some(poll) => ctx.reply(&poll.format_results()).await,
                None => ctx.reply("no active poll in this channel").await,
            }
        })
        .description("end the active poll and show results (mod only)")
        .category("moderation")
        .check(|ctx| async move {
            ctx.state::<Moderators>()
                .map(|m| m.contains(ctx.user().name.as_str()))
                .unwrap_or(false)
        })
        .on_check_fail(|ctx| async move {
            ctx.reply("only the streamer can end polls").await
        }),
    );
}
