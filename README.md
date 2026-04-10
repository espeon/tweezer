# tweezer

A chat bot framework for Rust. Platform-agnostic core, adapter crates for each platform.

## crates

- `tweezer`: bot runtime, handler registration, event types
- `tweezer-console`: stdin/stdout adapter for testing
- `tweezer-streamplace`: [Streamplace](https://stream.place) adapter (Jetstream)
- `tweezer-example-bot`: working example

## quick start

```rust
use tweezer::{prelude::*, Command};
use tweezer_console::ConsoleAdapter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut bot = Bot::new();
    bot.add_adapter(ConsoleAdapter::new());

    bot.add_command(Command::new("ping", |ctx: Context| async move {
        ctx.reply("pong").await
    }));

    bot.add_command(Command::new("echo", |ctx: Context| async move {
        let text = ctx.args().join(" ");
        ctx.reply(&text).await
    }));

    bot.on_raid(|ctx| async move {
        ctx.reply("welcome raiders!").await
    });

    bot.run().await?;
    Ok(())
}
```

## commands

Commands match on a prefix (`!` by default, configurable via `Bot::command_prefix`).

```rust
use tweezer::{Bot, Command};

// global command, fires on all channels
bot.add_command(Command::new("help", |ctx: Context| async move {
    ctx.reply("commands: !ping !echo !help").await
}));

// scoped to one channel
bot.add_command(Command::new("timer", |ctx: Context| async move {
    ctx.reply("counting down...").await
}).channel("did:plc:abc"));

// with description (shown by help_command) and category
bot.add_command(Command::new("ping", |ctx: Context| async move {
    ctx.reply("pong").await
}).description("responds with pong").category("general"));
```

`ctx.args()` returns the parsed arguments after the command name. Empty for `on_raw_message` handlers.

```rust
bot.add_command(Command::new("ban", |ctx: Context| async move {
    let target = ctx.args().first().map(|s| s.as_str()).unwrap_or("nobody");
    ctx.reply(&format!("banned {}", target)).await
}));
// !ban alice -> "banned alice"
```

### auto-generated help

`bot.help_command()` registers a `!help` command that lists all commands with descriptions, grouped by category. Channel-scoped commands are only shown when the message comes from that channel.

```rust
bot.add_command(Command::new("ping", |ctx: Context| async { Ok(()) }).description("responds with pong").category("general"));
bot.add_command(Command::new("timer", |ctx: Context| async { Ok(()) }).description("what is the timer?").channel("did:web:stream.place"));
bot.help_command();
// !help -> shows ping (global) and timer (if on the right channel), grouped by category
```

## raw message handlers

```rust
bot.on_raw_message(|ctx| async move {
    println!("[{}] {}: {}", ctx.channel(), ctx.user().name, ctx.message);
    Ok(())
});

bot.on_raw_message_for("my-channel", |ctx| async move {
    ctx.reply("channel-specific response").await
});

bot.on_raw_message_platform("streamplace", |ctx| async move {
    ctx.reply("streamplace-specific response").await
});
```

## triggers

```rust
bot.on_raid(|ctx| async move { ctx.reply("welcome raiders!").await });
bot.on_follow(|ctx| async move { ctx.reply("thanks for following!").await });
bot.on_subscription(|ctx| async move { ctx.reply("welcome subscriber!").await });
bot.on_donation(|ctx| async move { ctx.reply("thanks for the donation!").await });

// catch-all
bot.on_trigger(|ctx| async move {
    println!("trigger on {}", ctx.channel());
    Ok(())
});
```

Platform-specific triggers use the `PlatformTrigger` trait. See `tweezer-console` for an example.

## configuration

```rust
let mut bot = Bot::new()
    .command_prefix('/')
    .max_concurrency(64); // default 256
```

## shutdown

```rust
let handle = bot.shutdown_handle();

tokio::spawn(async move {
    tokio::signal::ctrl_c().await.unwrap();
    handle.shutdown();
});

bot.run().await?; // returns cleanly
```

## errors

```rust
bot.on_error(|e| {
    tracing::error!(platform = %e.platform, channel = %e.channel, error = %e.error);
});
```

`e.kind` is `Command { name }`, `Message`, or `Trigger`. `e.platform` and `e.channel` identify the source.

## streamplace

One Jetstream connection, multiple streamers:

```rust
use tweezer_streamplace::StreamplaceAdapter;

let mut adapter = StreamplaceAdapter::new(
    "wss://jetstream.firehose.cam",
    "mycool.bot",  // handle or DID
    "password",
);
adapter.add_streamer("did:plc:streamer-a");
adapter.add_streamer("did:plc:streamer-b");
adapter.add_streamers(["did:plc:streamer-c", "did:plc:streamer-d"]);

bot.add_adapter(adapter);
```

## writing an adapter

Implement the `Adapter` trait:

```rust
struct MyAdapter;

#[async_trait]
impl Adapter for MyAdapter {
    fn platform_name(&self) -> &str { "my-platform" }

    fn emote_fn(&self) -> Arc<dyn Fn(&str) -> String + Send + Sync> {
        Arc::new(|name| format!(":{}:", name))
    }

    async fn connect(&mut self, bot: BotTx) -> Result<(), TweezerError> {
        let tx = bot.clone();
        tokio::spawn(async move {
            let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel(8);
            tokio::spawn(async move {
                while let Some(msg) = reply_rx.recv().await {
                    // send msg.text to platform
                }
            });
            tx.send(Event::Message(IncomingMessage {
                platform: "my-platform".into(),
                user: User { name: "alice".into(), id: "42".into() },
                text: "hello".into(),
                channel: "general".into(),
                reply_tx,
                emote_fn: Arc::new(|name| format!(":{}:", name)),
            })).await.ok();
        });
        Ok(())
    }
    //...
}
```

## stream overlay

Use tweezer to forward chat messages to a WebSocket so a browser overlay can display them:

```rust
use tokio::sync::broadcast;

let (overlay_tx, _) = broadcast::channel::<String>(256);

// handler sends each message as JSON
let tx = overlay_tx.clone();
bot.on_raw_message(move |ctx| {
    let tx = tx.clone();
    async move {
        let json = serde_json::json!({
            "user": ctx.user().name,
            "text": ctx.message,
            "platform": ctx.platform(),
        });
        let _ = tx.send(json.to_string());
        Ok(())
    }
});

// websocket server pushes to connected browsers
let overlay = async move {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:9123").await.unwrap();
    while let Ok((stream, _)) = listener.accept().await {
        let mut rx = overlay_tx.subscribe();
        tokio::spawn(async move {
            let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let (mut write, _) = ws.split();
            while let Ok(msg) = rx.recv().await {
                write.send(msg.into()).await.ok();
            }
        });
    }
};
tokio::spawn(overlay);
```
