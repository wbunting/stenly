//! The configured encrypted-share destination.
use super::{AutoRule, Delivered, Destination, DestinationConfig, Kind, Payload};
use anyhow::{Result, bail};

pub struct ShareDestination { d: DestinationConfig }
impl ShareDestination { pub fn new(d: DestinationConfig) -> Self { Self { d } } }

impl Destination for ShareDestination {
    fn id(&self) -> &str { &self.d.id }
    fn name(&self) -> &str { &self.d.name }
    fn kind(&self) -> Kind { Kind::Share }
    fn service(&self) -> String {
        self.d.share.server_url.trim().trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/').to_string()
    }
    fn updates_in_place(&self) -> bool { true }
    fn ready(&self) -> Result<()> {
        let url = self.d.share.server_url.trim();
        if url.is_empty() { bail!("add the share server URL") }
        if !(url.starts_with("https://") || url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost")) { bail!("the share server URL should start with https://") }
        Ok(())
    }
    fn test(&self) -> Result<String> { self.ready()?; crate::share::test(&self.d) }
    fn send(&self, p: &Payload, _previous: Option<&str>) -> Result<Delivered> {
        let share = crate::share::upload(&self.d, p)?;
        crate::share::record(&p.dir, share.clone())?;
        // Never put the fragment key in a SendRecord / meta.json. UI and CLI
        // callers derive the keyed link from the Keychain after this returns.
        Ok(Delivered { url: Some(share.url), remote_id: Some("share".into()), detail: Some("Private link created".into()), clipboard: None })
    }
    fn auto(&self) -> &AutoRule { &self.d.auto }
}
