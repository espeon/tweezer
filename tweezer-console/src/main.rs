use tweezer::{prelude::*, Command};
use tweezer_console::ConsoleAdapter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    let mut adapter = ConsoleAdapter::new();
    if let Some(platform) = args.get(1) {
        adapter = adapter.masquerade(platform);
    }
    if let Some(username) = args.get(2) {
        adapter = adapter.username(username);
    }

    eprintln!(
        "tweezer console -- platform: {}\ncommands: !help !ping !whoami !platform\ntriggers: /trigger raid|follow|sub|donation|platform [key=value ...]",
        adapter.platform_name()
    );

    let mut bot = Bot::new();
    bot.add_adapter(adapter);

    bot.add_command(Command::new("help", |ctx: Context| async move {
        ctx.reply("commands: !help !ping !whoami !platform | triggers: /trigger raid|follow|sub|donation|platform").await
    }));

    bot.add_command(Command::new("ping", |ctx: Context| async move {
        ctx.reply("pong").await
    }));

    bot.add_command(Command::new("whoami", |ctx: Context| async move {
        let msg = format!("you are {} (id: {})", ctx.user().name, ctx.user().id);
        ctx.reply(&msg).await
    }));

    bot.add_command(Command::new("platform", |ctx: Context| async move {
        let msg = format!("platform: {}, channel: {}", ctx.platform(), ctx.channel());
        ctx.reply(&msg).await
    }));

    bot.run().await?;
    Ok(())
}
