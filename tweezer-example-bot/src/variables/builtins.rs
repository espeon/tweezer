use async_trait::async_trait;
use tweezer::Context;

use super::VariableProvider;

pub struct BuiltinProvider;

#[async_trait]
impl VariableProvider for BuiltinProvider {
    async fn resolve(
        &self,
        name: &str,
        args: &str,
        ctx: &Context,
        _cmd_name: &str,
    ) -> Option<String> {
        match name {
            "user" => Some(ctx.user().display().to_string()),
            "channel" => Some(ctx.channel().to_string()),
            "platform" => Some(ctx.platform().to_string()),
            "touser" => Some(
                ctx.args()
                    .first()
                    .map(|s| s.as_str())
                    .unwrap_or_else(|| ctx.user().display())
                    .to_string(),
            ),
            "random" => {
                // args: "N-M"
                let (lo, hi) = parse_range(args)?;
                let n = lo + (rand_u64() % (hi - lo + 1) as u64) as i64;
                Some(n.to_string())
            }
            "urlfetch" => {
                let url = args.trim();
                if url.is_empty() {
                    return Some("(no url)".into());
                }
                match fetch(url).await {
                    Ok(body) => {
                        let trimmed = body.trim().to_string();
                        // cap at 400 chars to avoid flooding chat
                        if trimmed.len() > 400 {
                            Some(format!("{}…", &trimmed[..400]))
                        } else {
                            Some(trimmed)
                        }
                    }
                    Err(e) => Some(format!("(fetch error: {e})")),
                }
            }
            n if n.parse::<usize>().is_ok() => {
                let idx: usize = n.parse().unwrap();
                Some(
                    ctx.args()
                        .get(idx.saturating_sub(1))
                        .map(|s| s.as_str())
                        .unwrap_or("")
                        .to_string(),
                )
            }
            _ => None,
        }
    }
}

fn parse_range(s: &str) -> Option<(i64, i64)> {
    // Find the '-' that separates the two numbers: a '-' preceded by a digit.
    // This correctly handles negative bounds like "-5-5" or "-10--3".
    let bytes = s.as_bytes();
    let sep = bytes
        .iter()
        .enumerate()
        .skip(1)
        .find(|(i, b)| **b == b'-' && bytes[i - 1].is_ascii_digit())
        .map(|(i, _)| i)?;
    let lo = s[..sep].trim().parse::<i64>().ok()?;
    let hi = s[sep + 1..].trim().parse::<i64>().ok()?;
    if lo > hi {
        return None;
    }
    Some((lo, hi))
}

/// A simple xorshift* PRNG. **Not cryptographically secure**
fn rand_u64() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    let mut x = seed.wrapping_add(0x9e3779b97f4a7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

async fn fetch(url: &str) -> Result<String, String> {
    reqwest::get(url)
        .await
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_range_valid() {
        assert_eq!(parse_range("1-10"), Some((1, 10)));
        assert_eq!(parse_range("0-0"), Some((0, 0)));
        assert_eq!(parse_range("-5-5"), Some((-5, 5)));
    }

    #[test]
    fn parse_range_inverted_returns_none() {
        assert!(parse_range("10-1").is_none());
    }

    #[test]
    fn parse_range_non_numeric_returns_none() {
        assert!(parse_range("a-b").is_none());
        assert!(parse_range("").is_none());
    }
}
