use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

// ---------------------------------------------------------------------------
// Active poll
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct PollOption {
    pub label: String,
    pub voters: HashSet<String>,
}

#[derive(Clone)]
pub struct ActivePoll {
    pub question: String,
    pub options: Vec<PollOption>,
}

impl ActivePoll {
    pub fn results(&self) -> Vec<(String, usize)> {
        self.options
            .iter()
            .map(|o| (o.label.clone(), o.voters.len()))
            .collect()
    }

    pub fn total_votes(&self) -> usize {
        self.options.iter().map(|o| o.voters.len()).sum()
    }

    pub fn vote(&mut self, user_id: &str, choice: usize) -> Result<(), &'static str> {
        if choice == 0 || choice > self.options.len() {
            return Err("invalid option");
        }
        // Remove user from all options first (one vote per user)
        for opt in &mut self.options {
            opt.voters.remove(user_id);
        }
        self.options[choice - 1].voters.insert(user_id.to_string());
        Ok(())
    }

    pub fn format_results(&self) -> String {
        let total = self.total_votes();
        let mut parts = vec![format!("📊 {} ({} votes)", self.question, total)];
        for (i, (label, count)) in self.results().iter().enumerate() {
            let pct = if total > 0 {
                (*count as f64 / total as f64 * 100.0).round() as u32
            } else {
                0
            };
            let bar_len = if total > 0 {
                (*count as f64 / total as f64 * 10.0).round() as usize
            } else {
                0
            };
            let bar = "█".repeat(bar_len);
            parts.push(format!("{} {}: {}% ({} votes) {}", i + 1, label, pct, count, bar));
        }
        parts.join(" | ")
    }
}

// ---------------------------------------------------------------------------
// Poll manager (per-channel)
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct PollManager {
    polls: Arc<Mutex<HashMap<String, ActivePoll>>>,
}

impl PollManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start(&self, channel: &str, question: &str, options: Vec<String>) -> Result<(), &'static str> {
        if options.len() < 2 {
            return Err("need at least 2 options");
        }
        if options.len() > 10 {
            return Err("max 10 options");
        }
        let poll = ActivePoll {
            question: question.to_string(),
            options: options
                .into_iter()
                .map(|label| PollOption {
                    label,
                    voters: HashSet::new(),
                })
                .collect(),
        };
        self.polls.lock().unwrap().insert(channel.to_string(), poll);
        Ok(())
    }

    pub fn end(&self, channel: &str) -> Option<ActivePoll> {
        self.polls.lock().unwrap().remove(channel)
    }

    pub fn get(&self, channel: &str) -> Option<ActivePoll> {
        self.polls.lock().unwrap().get(channel).cloned()
    }

    pub fn vote(&self, channel: &str, user_id: &str, choice: usize) -> Result<(), &'static str> {
        let mut polls = self.polls.lock().unwrap();
        let poll = polls.get_mut(channel).ok_or("no active poll")?;
        poll.vote(user_id, choice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vote_and_results() {
        let mgr = PollManager::new();
        mgr.start("ch1", "best color?", vec!["red".into(), "blue".into()]).unwrap();
        mgr.vote("ch1", "alice", 1).unwrap();
        mgr.vote("ch1", "bob", 2).unwrap();
        mgr.vote("ch1", "alice", 2).unwrap(); // change vote

        let poll = mgr.get("ch1").unwrap();
        let results = poll.results();
        assert_eq!(results[0].1, 0);
        assert_eq!(results[1].1, 2);
    }

    #[test]
    fn end_removes_poll() {
        let mgr = PollManager::new();
        mgr.start("ch1", "q", vec!["a".into(), "b".into()]).unwrap();
        assert!(mgr.end("ch1").is_some());
        assert!(mgr.get("ch1").is_none());
    }

    #[test]
    fn vote_without_poll_fails() {
        let mgr = PollManager::new();
        assert!(mgr.vote("ch1", "alice", 1).is_err());
    }
}
