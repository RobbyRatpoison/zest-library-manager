//! The bundled controller layouts (SDL_GameControllerDB, zlib licence next to the data file; generated for PlayDate by `tools/build_gamepad_db.py`, Linux entries only, keyed by USB
//! `vendor:product`). gilrs knows many pads by itself; for one it reports as "from the driver (unmapped)" the layout here can be turned into an SDL mapping line and saved.

use std::sync::OnceLock;

static DB: OnceLock<serde_json::Value> = OnceLock::new();

fn db() -> &'static serde_json::Value {
    DB.get_or_init(|| serde_json::from_str(include_str!("../data/gamepad_layouts.json")).unwrap_or(serde_json::Value::Null))
}

/// The database's name and first layout (`a:b0,b:b1,...`) for a USB vendor and product id.
pub fn lookup(vendor: u16, product: u16) -> Option<(String, String)> {
    let e = db().get("layouts")?.get(format!("{vendor:04x}:{product:04x}"))?;
    let fields = e.get("maps")?.as_array()?.first()?.as_str()?.to_string();
    Some((e.get("name").and_then(|n| n.as_str()).unwrap_or("Controller").to_string(), fields))
}

/// A full SDL mapping line for a pad: its GUID (32 hex digits), the database name, the layout, and the platform.
pub fn mapping_line(uuid_hex: &str, vendor: u16, product: u16) -> Option<String> {
    let (name, fields) = lookup(vendor, product)?;
    Some(format!("{uuid_hex},{name},{fields},platform:Linux,"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_8bitdo_ultimate_2_has_a_layout_with_x_and_y_in_order() {
        let (name, fields) = lookup(0x2dc8, 0x310b).expect("2dc8:310b is in the database");
        assert!(name.contains("8BitDo"));
        assert!(fields.contains("x:b2") && fields.contains("y:b3") && fields.contains("lefttrigger:a2"));
        let line = mapping_line("03000000c82d00000b31000014010000", 0x2dc8, 0x310b).unwrap();
        assert!(line.starts_with("03000000c82d00000b31000014010000,8BitDo") && line.ends_with("platform:Linux,"));
    }
    #[test]
    fn an_unknown_pad_has_no_layout() { assert!(lookup(0x1234, 0x5678).is_none()); }
}
