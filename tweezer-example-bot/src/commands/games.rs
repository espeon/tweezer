use rand::{Rng, SeedableRng};
use tweezer::{Bot, Command, Context};

fn parse_dice_spec(s: &str) -> Option<(u32, u32)> {
    let s = s.to_lowercase();
    let parts: Vec<&str> = s.split('d').collect();
    if parts.len() != 2 {
        return None;
    }
    let count = if parts[0].is_empty() {
        1
    } else {
        parts[0].parse().ok()?
    };
    let sides: u32 = parts[1].parse().ok()?;
    Some((count, sides))
}

pub fn add_commands(bot: &mut Bot) {
    bot.add_command(
        Command::new("roll", |ctx: Context| async move {
            let spec = ctx.args().first().map(|s| s.as_str()).unwrap_or("1d6");
            let (count, sides) = match parse_dice_spec(spec) {
                Some(v) => v,
                None => return ctx.reply("usage: !roll [N]dM  (e.g. !roll 2d20, !roll d6)").await,
            };
            if count == 0 || sides == 0 {
                return ctx.reply("need at least 1 die with at least 1 side").await;
            }
            if count > 100 {
                return ctx.reply("max 100 dice").await;
            }
            let mut rng = rand::rngs::StdRng::from_entropy();
            let rolls: Vec<u32> = (0..count).map(|_| rng.gen_range(1..=sides)).collect();
            let total: u32 = rolls.iter().sum();
            let text = if rolls.len() == 1 {
                format!("rolled {total}")
            } else {
                format!(
                    "rolled {} = {}",
                    rolls
                        .iter()
                        .map(|r| r.to_string())
                        .collect::<Vec<_>>()
                        .join(" + "),
                    total
                )
            };
            ctx.reply(&text).await
        })
        .description("roll dice: !roll 2d20, !roll d6")
        .category("games"),
    );

    bot.add_command(
        Command::new("coin", |ctx: Context| async move {
            let mut rng = rand::rngs::StdRng::from_entropy();
            let result = if rng.gen_bool(0.5) { "heads" } else { "tails" };
            ctx.reply(result).await
        })
        .description("flip a coin")
        .category("games"),
    );
}
