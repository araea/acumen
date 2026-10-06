use crate::adapters::satori::LockedWriter;
use crate::plugins::PluginConfig;
use crate::event::Context;
use crate::plugins::PluginError;
use futures_util::future::BoxFuture;
use serde::Serialize;

#[derive(Serialize, serde::Deserialize)]
#[serde(default)]
pub struct FilterConfig {
    enabled: bool,
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl PluginConfig for FilterConfig {
    const NAME: &'static str = "meta_filter";
}


pub fn handle(
    ctx: Context,
    _writer: LockedWriter,
) -> BoxFuture<'static, Result<Option<Context>, PluginError>> {
    Box::pin(async move {
        if let Some(post_type) = ctx.post_type()
            && post_type == "meta_event"
        {
            return Ok(None);
        }
        Ok(Some(ctx))
    })
}

