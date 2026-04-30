use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tweezer::{Bot, Command, Context, TweezerError};

use crate::db::Database;

// ---------------------------------------------------------------------------
// Duration parsing
// ---------------------------------------------------------------------------

fn parse_duration(input: &str) -> Option<Duration> {
    let mut total_secs: u64 = 0;
    let mut num = String::new();

    for ch in input.chars() {
        if ch.is_ascii_digit() {
            num.push(ch);
        } else if ch == 'h' || ch == 'H' {
            let n: u64 = num.parse().ok()?;
            total_secs += n * 3600;
            num.clear();
        } else if ch == 'm' || ch == 'M' {
            let n: u64 = num.parse().ok()?;
            total_secs += n * 60;
            num.clear();
        } else if ch == 's' || ch == 'S' {
            let n: u64 = num.parse().ok()?;
            total_secs += n;
            num.clear();
        } else if ch.is_whitespace() {
            continue;
        } else {
            return None;
        }
    }

    if total_secs == 0 && num.parse::<u64>().unwrap_or(0) > 0 {
        // Bare number, treat as minutes
        total_secs = num.parse::<u64>().ok()? * 60;
    }

    if total_secs > 0 {
        Some(Duration::from_secs(total_secs))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Reminder manager
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ReminderManager {
    db: Database,
}

impl ReminderManager {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    pub async fn check_due(&self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let due = match self.db.reminder_due(now) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("failed to check reminders: {e}");
                return;
            }
        };

        for (_id, _channel, _user_id, user_name, message) in due {
            // We can't send proactive messages without a reply_tx.
            // For now, reminders are loaded but can't fire after restart.
            // This is a known limitation.
            tracing::info!("reminder due for {user_name}: {message} (skipped, no reply channel)");
        }

        if let Err(e) = self.db.reminder_delete_due(now) {
            tracing::warn!("failed to clean up reminders: {e}");
        }
    }

    pub fn add(
        &self,
        channel: &str,
        user_id: &str,
        user_name: &str,
        message: &str,
        duration: Duration,
    ) -> Result<usize, TweezerError> {
        let trigger_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            + duration.as_secs();

        self.db
            .reminder_add(channel, user_id, user_name, message, trigger_at)
            .map_err(|e| TweezerError::Handler(format!("db error: {e}")))
    }

    pub fn count(&self) -> usize {
        self.db.reminder_count().unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

pub fn add_commands(bot: &mut Bot) {
    bot.add_command(
        Command::new("remindme", |ctx: Context| async move {
            let args = ctx.args();
            if args.len() < 2 {
                return ctx.reply("usage: !remindme <duration> <message>  (e.g. !remindme 15m check oven)").await;
            }

            let duration = match parse_duration(&args[0]) {
                Some(d) => d,
                None => return ctx.reply("can't parse duration. use formats like: 15m, 2h, 1h30m").await,
            };

            if duration.as_secs() < 60 {
                return ctx.reply("minimum reminder is 1 minute").await;
            }
            if duration.as_secs() > 86400 * 7 {
                return ctx.reply("max reminder is 7 days").await;
            }

            let message = args[1..].join(" ");
            let mgr = match ctx.state::<ReminderManager>() {
                Some(m) => m,
                None => return ctx.reply("reminder system not available").await,
            };

            match mgr.add(ctx.channel(), &ctx.user().id, &ctx.user().name, &message, duration) {
                Ok(id) => {
                    let mins = duration.as_secs() / 60;
                    let time_str = if mins >= 60 {
                        format!("{}h {}m", mins / 60, mins % 60)
                    } else {
                        format!("{mins}m")
                    };
                    ctx.reply(&format!("reminder #{id} set for {time_str}: {message}")).await
                }
                Err(e) => ctx.reply(&format!("failed to set reminder: {e}")).await,
            }
        })
        .description("set a reminder (e.g. !remindme 15m check oven)")
        .category("general"),
    );

    bot.add_command(
        Command::new("reminders", |ctx: Context| async move {
            let mgr = match ctx.state::<ReminderManager>() {
                Some(m) => m,
                None => return ctx.reply("reminder system not available").await,
            };
            let count = mgr.count();
            ctx.reply(&format!("{count} active reminders")).await
        })
        .description("count active reminders")
        .category("general"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_15m() {
        assert_eq!(parse_duration("15m"), Some(Duration::from_secs(900)));
    }

    #[test]
    fn parse_2h() {
        assert_eq!(parse_duration("2h"), Some(Duration::from_secs(7200)));
    }

    #[test]
    fn parse_1h30m() {
        assert_eq!(parse_duration("1h30m"), Some(Duration::from_secs(5400)));
    }

    #[test]
    fn parse_bare_number_as_minutes() {
        assert_eq!(parse_duration("30"), Some(Duration::from_secs(1800)));
    }

    #[test]
    fn parse_invalid() {
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration(""), None);
    }
}
