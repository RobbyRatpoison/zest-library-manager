//! Settings > Advanced > Send Log to Developer: its own dialog (it used to be a section of Data). Sends Zest's log with credentials removed, the version, the system and the settings
//! snapshot through the relay, with an optional message; one report per five minutes.
use crate::ui_kit::{self, Kind};
use std::path::PathBuf;
use std::sync::mpsc;

pub enum SendLogOut { None, Close, /// A report was sent: its time goes into `state.json` for the cooldown.
    Sent }

pub struct SendLogDialog {
    root: PathBuf, db: PathBuf, msg: String, last_sent: f64, busy: bool, status: Option<(String, bool)>,
    tx: mpsc::Sender<Result<String, String>>, rx: mpsc::Receiver<Result<String, String>>, ctx: egui::Context,
}

impl SendLogDialog {
    /// `last_sent`: when a report was last sent (`last_log_submit_at`), for the wait between reports.
    pub fn new(ctx: &egui::Context, root: PathBuf, db: PathBuf, last_sent: f64) -> Self {
        let (tx, rx) = mpsc::channel();
        SendLogDialog { root, db, msg: String::new(), last_sent, busy: false, status: None, tx, rx, ctx: ctx.clone() }
    }
    fn send(&mut self) {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
        let wait = (pd_filter::sendlog::COOLDOWN_SECS as f64 - (now - self.last_sent)).ceil();
        if self.last_sent > 0.0 && wait > 0.0 { self.status = Some((format!("Please wait {}s before sending again.", wait as i64), false)); return; }
        self.busy = true; self.status = Some(("Sending...".into(), true));
        let (root, db, msg, tx, ctx) = (self.root.clone(), self.db.clone(), self.msg.clone(), self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let r = (|| -> Result<String, String> {
                let log = std::fs::read_to_string(root.join("zest.log")).unwrap_or_default();
                let state: serde_json::Value = std::fs::read_to_string(root.join("state.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(serde_json::json!({}));
                let conn = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok();
                let snap = pd_filter::sendlog::snapshot(&state, conn.as_ref(), &[]);
                let os = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
                pd_filter::sendlog::send(&pd_filter::net::UreqHttp::new(), pd_filter::sendlog::RELAY_URL, &msg, pd_filter::updater::VERSION, &os, &log, &snap)?;
                Ok("Sent. Thank you.".to_string())
            })();
            let _ = tx.send(r); ctx.request_repaint();
        });
    }
    pub fn show(&mut self, ctx: &egui::Context, accent: egui::Color32) -> SendLogOut {
        let mut out = SendLogOut::None;
        while let Ok(r) = self.rx.try_recv() {
            self.busy = false;
            match r {
                Ok(s) => { self.status = Some((s, true)); self.msg.clear(); self.last_sent = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64()); out = SendLogOut::Sent; }
                Err(e) => self.status = Some((e, false)),
            }
        }
        let mut send = false;
        let res = ui_kit::modal(ctx, "send_log", "Send Log to Developer", 36.0, |ui, r| {
            ui_kit::style_fields(ui);
            ui.label(ui_kit::muted("Sends Zest's log (credentials removed), the version, your system and your settings to the developer, to help find a problem. Your Steam key and other sign-ins are never sent. Nothing is sent until you press the button.", 0.85 * r));
            ui.add_space(0.5 * r);
            ui.add(egui::TextEdit::multiline(&mut self.msg).hint_text("What went wrong? (optional)").desired_rows(4).desired_width(f32::INFINITY).font(egui::FontId::proportional(0.9 * r)));
            ui.add_space(0.4 * r);
            ui.horizontal(|ui| {
                if ui_kit::button(ui, "SEND LOG", Kind::Primary, accent, r).clicked() && !self.busy { send = true; }
                if ui_kit::button(ui, "CLOSE", Kind::Plain, accent, r).clicked() { out = SendLogOut::Close; }
            });
            if let Some((m, ok)) = &self.status { ui.add_space(0.3 * r); ui.label(egui::RichText::new(m).size(0.85 * r).color(if *ok { ui_kit::positive() } else { ui_kit::text_danger() })); }
        });
        if send { self.send(); }
        if res.closed && matches!(out, SendLogOut::None) { out = SendLogOut::Close; }
        out
    }
}
