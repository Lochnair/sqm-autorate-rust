#[derive(Debug, thiserror::Error)]
#[error("channel closed")]
pub(crate) struct ChannelClosed;
