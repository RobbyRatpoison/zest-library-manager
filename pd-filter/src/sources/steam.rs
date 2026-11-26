//! Steam as a library source: the store, launching through the Steam client, install state from the local manifests, metadata from the store and review
//! endpoints. The fetching itself is `steamclient` / `jobs::rescrape_steam`; this is the interface around it.
use super::{Action, Caps, GameRef, Launch, LibrarySource, SourceCtx};
use crate::jobs::{rescrape_steam, Update};
use crate::steamclient::Fetch;

pub struct SteamSource;

impl LibrarySource for SteamSource {
    fn id(&self) -> &str { "steam" }
    fn label(&self) -> &str { "Steam" }
    fn caps(&self) -> Caps { Caps { sync: true, rescrape: true, launch: true, uninstall: true, installed_scan: true, description: true, art: true, store_page: true, achievements_page: true, community_page: true, ..Default::default() } }

    fn store_url(&self, g: &GameRef) -> Option<String> { Some(format!("https://store.steampowered.com/app/{}", g.appid)) }
    fn achievements_url(&self, g: &GameRef, account_id: Option<&str>) -> Option<String> { account_id.map(|id| format!("https://steamcommunity.com/profiles/{id}/stats/{}/achievements/", g.appid)) }
    fn community_url(&self, g: &GameRef) -> Option<String> { Some(format!("https://steamcommunity.com/app/{}", g.appid)) }

    fn launch(&self, g: &GameRef) -> Result<Launch, String> { Ok(Launch { action: Action::OpenUrl(format!("steam://run/{}", g.appid)), record: true }) }
    fn uninstall(&self, g: &GameRef) -> Result<Action, String> { Ok(Action::OpenUrl(format!("steam://uninstall/{}", g.appid))) }

    fn rescrape(&self, cx: &SourceCtx, g: &GameRef) -> Fetch<Option<Update>> { rescrape_steam(&cx.steam, g.appid, (cx.appinfo)(g.appid), &cx.today) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn links_and_launch_match_what_the_app_did_before() {
        let g = GameRef { appid: 620, platform: "steam".into(), ..Default::default() };
        let s = SteamSource;
        assert_eq!(s.store_url(&g).unwrap(), "https://store.steampowered.com/app/620");
        assert_eq!(s.community_url(&g).unwrap(), "https://steamcommunity.com/app/620");
        assert_eq!(s.achievements_url(&g, Some("7656")).unwrap(), "https://steamcommunity.com/profiles/7656/stats/620/achievements/");
        assert!(s.achievements_url(&g, None).is_none(), "no account, no profile link");
        assert_eq!(s.launch(&g).unwrap(), Launch { action: Action::OpenUrl("steam://run/620".into()), record: true });
        assert_eq!(s.uninstall(&g).unwrap(), Action::OpenUrl("steam://uninstall/620".into()));
        let c = s.caps(); assert!(c.launch && c.rescrape && c.store_page && !c.install);
    }
}
