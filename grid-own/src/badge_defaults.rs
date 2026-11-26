//! The store logos PlayDate ships in `static/img/platform/` for the card badge's "bundled badge" setting (Settings > Badges > "No icon for a store"), built into the program so a
//! Zest that has no PlayDate folders still has them. A store with no logo here shows its name as text instead.

/// The logo file of `platform`, if one is bundled.
pub fn bytes(platform: &str) -> Option<&'static [u8]> {
    Some(match platform {
        "amazon_games" => include_bytes!("../../assets/platform/amazon_games.png"),
        "battle_net" => include_bytes!("../../assets/platform/battle_net.png"),
        "custom" => include_bytes!("../../assets/platform/custom.png"),
        "ea_app" => include_bytes!("../../assets/platform/ea_app.png"),
        "epic_games" => include_bytes!("../../assets/platform/epic_games.png"),
        "gog" => include_bytes!("../../assets/platform/gog.png"),
        "humble" => include_bytes!("../../assets/platform/humble.png"),
        "indiegala" => include_bytes!("../../assets/platform/indiegala.png"),
        "itch_io" => include_bytes!("../../assets/platform/itch_io.png"),
        "legacy_games" => include_bytes!("../../assets/platform/legacy_games.png"),
        "nintendo" => include_bytes!("../../assets/platform/nintendo.png"),
        "playstation" => include_bytes!("../../assets/platform/playstation.png"),
        "rockstar" => include_bytes!("../../assets/platform/rockstar.png"),
        "steam" => include_bytes!("../../assets/platform/steam.png"),
        "ubisoft" => include_bytes!("../../assets/platform/ubisoft.png"),
        "xbox" => include_bytes!("../../assets/platform/xbox.png"),
        _ => return None,
    })
}

/// The logo decoded and cut to a circle (PlayDate's `card-badge-circle`; the Manual game icon, `custom`, keeps its own rounded-square shape, which a circle would chop).
pub fn decode(platform: &str) -> Option<image::RgbaImage> {
    let mut im = image::load_from_memory(bytes(platform)?).ok()?.to_rgba8();
    if platform == "custom" { return Some(im); }
    let (w, h) = (im.width() as f32, im.height() as f32);
    let (cx, cy, rad) = (w / 2.0, h / 2.0, w.min(h) / 2.0);
    for (x, y, p) in im.enumerate_pixels_mut() {
        let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
        let edge = (rad - d).clamp(0.0, 1.0);   // a one pixel soft edge
        p.0[3] = (p.0[3] as f32 * edge) as u8;
    }
    Some(im)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_logo_decodes_and_the_circle_clears_the_corners() {
        for p in ["amazon_games", "battle_net", "custom", "ea_app", "epic_games", "gog", "humble", "indiegala", "itch_io", "legacy_games", "nintendo", "playstation", "rockstar", "steam", "ubisoft", "xbox"] {
            let im = decode(p).unwrap_or_else(|| panic!("{p} does not decode"));
            assert!(im.width() > 0 && im.height() > 0);
            if p != "custom" { assert_eq!(im.get_pixel(0, 0).0[3], 0, "{p}: the corner should be cut away"); }
        }
        assert!(bytes("not_a_store").is_none());
    }
}
