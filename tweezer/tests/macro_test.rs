use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tweezer::{
    Adapter, Bot, BotTx, Command, Event, IncomingMessage, OutgoingMessage, TweezerError, User,
};

#[tweezer::command]
async fn ping(ctx: tweezer::Context) -> Result<(), TweezerError> {
    ctx.reply("pong").await
}

#[tweezer::command(name = "pong", aliases = ["p"])]
/// responds with ping
async fn macro_pong(ctx: tweezer::Context) -> Result<(), TweezerError> {
    ctx.reply("ping").await
}

#[tweezer::command(name = "scoped", channel = "special")]
async fn scoped_cmd(ctx: tweezer::Context) -> Result<(), TweezerError> {
    ctx.reply("special").await
}

struct DirectAdapter {
    events: Mutex<Vec<Event>>,
}

#[async_trait::async_trait]
impl Adapter for DirectAdapter {
    fn platform_name(&self) -> &str {
        "test"
    }
    fn emote_fn(&self) -> Arc<dyn Fn(&str) -> String + Send + Sync> {
        Arc::new(|n| format!(":{n}:"))
    }
    async fn connect(&mut self, bot: BotTx) -> Result<(), TweezerError> {
        let events = std::mem::take(&mut *self.events.lock().unwrap());
        for event in events {
            bot.send(event).await.ok();
        }
        Ok(())
    }
}

fn make_message(text: &str, reply_tx: mpsc::Sender<OutgoingMessage>) -> Event {
    Event::Message(
        IncomingMessage::new(
            "test",
            User {
                name: "alice".into(),
                id: "1".into(),
                display_name: None,
            },
            text,
            "general",
            reply_tx,
            Arc::new(|n| format!(":{n}:")),
        ),
    )
}

fn make_message_in(
    text: &str,
    channel: &str,
    reply_tx: mpsc::Sender<OutgoingMessage>,
) -> Event {
    Event::Message(
        IncomingMessage::new(
            "test",
            User {
                name: "alice".into(),
                id: "1".into(),
                display_name: None,
            },
            text,
            channel,
            reply_tx,
            Arc::new(|n| format!(":{n}:")),
        ),
    )
}

async fn run_with_events(bot: Bot, events: Vec<Event>) {
    let mut bot = bot;
    bot.add_adapter(DirectAdapter {
        events: Mutex::new(events),
    });
    bot.run().await.unwrap();
}

#[tokio::test]
async fn macro_command_runs() {
    let mut bot = Bot::new();
    bot.add_command(ping());

    let (reply_tx, mut reply_rx) = mpsc::channel(1);
    run_with_events(bot, vec![make_message("!ping", reply_tx)]).await;

    let msg = reply_rx.try_recv().expect("expected a reply");
    assert_eq!(msg.text, "pong");
}

#[tokio::test]
async fn macro_command_backward_compat_with_manual_command() {
    let mut bot = Bot::new();
    bot.add_command(Command::new("manual", |_ctx| async { Ok(()) }));
    bot.add_command(ping());

    let (reply_tx, mut reply_rx) = mpsc::channel(1);
    run_with_events(bot, vec![make_message("!ping", reply_tx)]).await;

    let msg = reply_rx.try_recv().expect("expected a reply");
    assert_eq!(msg.text, "pong");
}

#[tokio::test]
async fn macro_respects_name_override_and_alias() {
    let mut bot = Bot::new();
    bot.add_command(macro_pong());

    let (tx1, mut rx1) = mpsc::channel(1);
    let (tx2, mut rx2) = mpsc::channel(1);
    run_with_events(
        bot,
        vec![make_message("!pong", tx1), make_message("!p", tx2)],
    )
    .await;

    assert_eq!(rx1.try_recv().unwrap().text, "ping");
    assert_eq!(rx2.try_recv().unwrap().text, "ping");
}

#[tokio::test]
async fn macro_description_from_doc_comment() {
    let mut bot = Bot::new();
    bot.add_command(macro_pong());
    bot.help_command();

    let (tx1, _) = mpsc::channel(1);
    let (tx2, mut reply_rx) = mpsc::channel(4);
    run_with_events(
        bot,
        vec![make_message("!pong", tx1), make_message("!help pong", tx2)],
    )
    .await;

    let msg = reply_rx.try_recv().expect("expected help reply");
    assert!(msg.text.contains("!pong: responds with ping"));
}

#[tokio::test]
async fn macro_channel_scoping() {
    let mut bot = Bot::new();
    bot.add_command(scoped_cmd());

    let (tx1, mut rx1) = mpsc::channel(1);
    let (tx2, _) = mpsc::channel(1);
    run_with_events(
        bot,
        vec![
            make_message_in("!scoped", "special", tx1),
            make_message_in("!scoped", "other", tx2),
        ],
    )
    .await;

    assert_eq!(rx1.try_recv().unwrap().text, "special");
}
