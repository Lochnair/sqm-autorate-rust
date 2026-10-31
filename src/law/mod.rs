mod context;
mod sqma;

pub(crate) use context::LawContext;
pub(crate) use sqma::{ControlSnapshot, SqmaLaw};

#[async_trait::async_trait]
pub trait Law: Send {
    async fn run(self: Box<Self>, ctx: LawContext) -> anyhow::Result<()>;
}
