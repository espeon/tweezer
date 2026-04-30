#[derive(Clone, Debug, thiserror::Error)]
pub enum TweezerError {
    #[error("adapter connection failed: {0}")]
    Connection(String),
    #[error("failed to send reply: {0}")]
    Reply(String),
    #[error("trigger handler error: {0}")]
    Trigger(String),
    #[error("handler error: {0}")]
    Handler(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_error_display() {
        let err = TweezerError::Connection("refused".into());
        assert_eq!(err.to_string(), "adapter connection failed: refused");
    }

    #[test]
    fn reply_error_display() {
        let err = TweezerError::Reply("channel closed".into());
        assert_eq!(err.to_string(), "failed to send reply: channel closed");
    }

    #[test]
    fn trigger_error_display() {
        let err = TweezerError::Trigger("bad".into());
        assert_eq!(err.to_string(), "trigger handler error: bad");
    }

    #[test]
    fn handler_error_display() {
        let err = TweezerError::Handler("something went wrong".into());
        assert_eq!(err.to_string(), "handler error: something went wrong");
    }
}
