//! The Steam account form: the sign-in button, label, Steam ID with DETECT, and API key with SHOW. Settings > Account and the first-run window both draw it, so the two
//! always offer the same ways in: sign in with Steam, find the account on this computer, or type the ID and key.
use crate::ui_kit::{self, Kind};
use pd_filter::account::Account;
use pd_filter::steamlocal;
use pd_filter::steamsignin::{self, ApiKeyPage};
use std::sync::mpsc::{channel, Receiver};

type SignIn = Result<(String, ApiKeyPage, Option<String>), String>;

#[derive(Default)]
pub struct AccountForm {
    pub steam_id: String, pub api_key: String, pub label: String,
    /// What the form last did (DETECT's answer, the sign-in's result); the caller shows it, and takes it with `status.take()`.
    pub status: Option<(String, bool)>,
    show_key: bool, detected: Vec<steamlocal::Account>, sign_rx: Option<Receiver<SignIn>>,
}

impl AccountForm {
    pub fn from_account(a: Account) -> Self { AccountForm { steam_id: a.steam_id, api_key: a.api_key, label: a.label, ..Default::default() } }
    pub fn clear(&mut self) { self.steam_id.clear(); self.api_key.clear(); self.label.clear(); self.detected.clear(); }
    pub fn busy(&self) -> bool { self.sign_rx.is_some() }

    /// DETECT: the Steam accounts signed in on this computer. One fills the ID (and the label when empty), several are listed to pick from. `quiet`: say nothing when none is found.
    pub fn detect(&mut self, quiet: bool) {
        let found = steamlocal::steam_root(None).map(|root| steamlocal::accounts(&root)).unwrap_or_default();
        match found.len() {
            0 => { self.detected.clear(); if !quiet { self.status = Some(("Could not find Steam on this computer. Sign in with Steam or type the number in.".into(), false)); } }
            1 => { self.steam_id = found[0].steam_id.clone(); if self.label.is_empty() { self.label = found[0].name.clone(); } self.detected.clear(); self.status = Some((format!("Found {}.", found[0].name), true)); }
            _ => { self.detected = found; self.status = Some(("More than one Steam account is signed in on this computer. Pick yours below.".into(), true)); }
        }
    }

    /// Call once a frame: when a sign-in has finished it fills the fields and returns true (the status holds what Steam said about the key). `known` = the accounts already saved, so
    /// signing in as one of them brings its own key back, and as anyone else starts with an empty key (another account's key must never carry over).
    pub fn poll(&mut self, known: &[(Account, bool)]) -> bool {
        let Some(rx) = &self.sign_rx else { return false };
        let Ok(res) = rx.try_recv() else { return false };
        self.sign_rx = None;
        match res {
            Err(e) => { self.status = Some((e, false)); false }
            Ok((id, page, name)) => {
                if id != self.steam_id {
                    let k = known.iter().find(|(a, _)| a.steam_id == id).map(|(a, _)| a.clone());
                    self.api_key = k.as_ref().map(|a| a.api_key.clone()).unwrap_or_default(); self.label = k.map(|a| a.label).unwrap_or_default();
                }
                self.steam_id = id; self.detected.clear();
                // the display name from the public profile, only into an empty label (never over one that was typed or came with a saved account)
                if self.label.trim().is_empty() { if let Some(n) = name { self.label = n; } }
                let note = match page {
                    ApiKeyPage::Key(k) => { self.api_key = k; "Signed in. Steam gave your key and it is filled in.".to_string() }
                    ApiKeyPage::NoKey => "Signed in. Steam has no API key for you yet: press GET A KEY, register one (any domain name works, for example localhost), then paste it here.".to_string(),
                    ApiKeyPage::Limited => "Signed in. Steam does not give keys to accounts that have not spent $5 yet. Your SteamID is filled in; local files still sync playtime.".to_string(),
                    ApiKeyPage::Unknown => "Signed in. Steam's key page did not answer as expected, so only your SteamID is filled in. Paste the key by hand.".to_string(),
                };
                self.status = Some((note, true));
                true
            }
        }
    }

    /// Draws the form. Returns an address to open in the browser (GET A KEY).
    pub fn ui(&mut self, ui: &mut egui::Ui, r: f32, accent: egui::Color32) -> Option<&'static str> {
        let mut open = None;
        let busy = self.busy();
        if pd_filter::weblogin::helper_path().is_some() {
            if ui_kit::button(ui, if busy { "WAITING FOR STEAM..." } else { "SIGN IN WITH STEAM" }, Kind::Primary, accent, r).clicked() && !busy {
                let (tx, rx) = channel(); self.sign_rx = Some(rx); let c = ui.ctx().clone();
                self.status = Some(("A Steam sign-in window opened. Sign in there; Zest never sees your password.".into(), true));
                std::thread::spawn(move || {
                    let r = match pd_filter::weblogin::helper_path() { Some(h) => { let http = pd_filter::net::UreqHttp::new(); steamsignin::sign_in(&h, &http, steamsignin::Purpose::Account).map(|(id, page)| { let mut name = steamsignin::fetch_profile_name(&http, &id); if name.is_none() { if let steamsignin::ApiKeyPage::Key(k) = &page { name = steamsignin::fetch_name_with_key(&http, k, &id); } } (id, page, name) }) }, None => Err("The sign-in window is not installed.".to_string()) };
                    let _ = tx.send(r); c.request_repaint();
                });
            }
        } else { ui.label(ui_kit::muted("The sign-in window is not installed; type the SteamID and key below.", 0.8 * r)); }
        if busy { ui.ctx().request_repaint_after(std::time::Duration::from_millis(300)); }
        ui.add_space(0.25 * r);
        ui.label(ui_kit::muted("Label (nickname)", 0.8 * r));
        ui.add(egui::TextEdit::singleline(&mut self.label).desired_width(f32::INFINITY).font(egui::FontId::proportional(0.95 * r)));
        ui.label(ui_kit::muted("Steam ID (the 17 digit number of your profile)", 0.8 * r));
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 7.0 * r).max(4.0 * r);
            ui.add(egui::TextEdit::singleline(&mut self.steam_id).desired_width(w).font(egui::FontId::proportional(0.95 * r)));
            if ui_kit::button(ui, "DETECT", Kind::Plain, accent, r).clicked() { self.detect(false); }
        });
        let mut chosen = None;
        for d in &self.detected { if ui_kit::button(ui, &format!("{}  ({})", d.name, d.steam_id), Kind::Plain, accent, r).clicked() { chosen = Some(d.clone()); } }
        if let Some(d) = chosen { self.steam_id = d.steam_id; if self.label.is_empty() { self.label = d.name; } self.detected.clear(); self.status = Some(("Chosen.".into(), true)); }
        ui.horizontal(|ui| { ui.label(ui_kit::muted("Steam API key", 0.8 * r)); ui.label(ui_kit::muted("(recommended, it lets a sync read your achievements and playtime)", 0.75 * r)); });
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 7.0 * r).max(4.0 * r);
            ui.add(egui::TextEdit::singleline(&mut self.api_key).password(!self.show_key).desired_width(w).font(egui::FontId::proportional(0.95 * r)));
            if ui_kit::button(ui, if self.show_key { "HIDE" } else { "SHOW" }, Kind::Plain, accent, r).clicked() { self.show_key = !self.show_key; }
        });
        if ui_kit::button(ui, "GET A KEY", Kind::Plain, accent, r).clicked() { open = Some(steamsignin::API_KEY_URL); }
        open
    }
}
