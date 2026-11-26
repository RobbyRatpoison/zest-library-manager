//! Checks the patched egui: a double click selects the word as the second button goes DOWN, and dragging from there grows the selection a word at a time.

#[cfg(test)]
mod tests {
    use egui::{Event, PointerButton, Pos2, Rect, text::{CCursor, CCursorRange}};

    fn frame(ctx: &egui::Context, t: f64, events: Vec<Event>, text: &mut String, id: egui::Id) -> Rect {
        let mut raw = egui::RawInput { time: Some(t), screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(600.0, 200.0))), events, ..Default::default() };
        raw.focused = true;
        let mut galley_rect = Rect::NOTHING;
        let _ = ctx.run_ui(raw, |ui| {
            let out = egui::TextEdit::singleline(text).id(id).desired_width(500.0).show(ui);
            galley_rect = out.galley_pos.to_vec2().to_pos2().to_owned().into_rect(out.galley.size());
        });
        galley_rect
    }

    trait IntoRect { fn into_rect(self, size: egui::Vec2) -> Rect; }
    impl IntoRect for Pos2 { fn into_rect(self, size: egui::Vec2) -> Rect { Rect::from_min_size(self, size) } }

    fn word_x(ctx: &egui::Context, text: &str, id: egui::Id, char_index: usize, origin: Pos2) -> Pos2 {
        // x of a character, from the layout the widget used
        let font = egui::FontId::proportional(14.0);
        let galley = ctx.fonts_mut(|f| f.layout_no_wrap(text.to_string(), font, egui::Color32::WHITE));
        let r = galley.pos_from_cursor(CCursor::new(char_index));
        let _ = id;
        Pos2::new(origin.x + r.min.x + 2.0, origin.y + 8.0)
    }

    fn selection(ctx: &egui::Context, id: egui::Id) -> Option<std::ops::Range<usize>> {
        egui::TextEdit::load_state(ctx, id).and_then(|s| s.cursor.char_range()).map(|r: CCursorRange| r.as_sorted_char_range())
    }

    #[test]
    fn double_press_selects_a_word_and_drag_extends_by_words() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|s| s.interaction.select_word_on_press = true);
        let id = egui::Id::new("t");
        let mut text = "alpha beta gamma delta".to_string();
        // lay out once to learn where the field sits
        let rect = frame(&ctx, 0.0, vec![], &mut text, id);
        let origin = rect.min;
        let p_beta = word_x(&ctx, &text, id, 8, origin);       // inside "beta"
        let p_delta = word_x(&ctx, &text, id, 19, origin);     // inside "delta"
        let press = |p: Pos2, pressed: bool| Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Default::default() };
        // first click
        frame(&ctx, 1.00, vec![Event::PointerMoved(p_beta), press(p_beta, true)], &mut text, id);
        frame(&ctx, 1.05, vec![press(p_beta, false)], &mut text, id);
        // second press: the word must be selected NOW, with the button still down
        frame(&ctx, 1.15, vec![press(p_beta, true)], &mut text, id);
        assert_eq!(selection(&ctx, id), Some(6..10), "word not selected on the second press");
        // drag right to 'delta': the selection grows to whole words
        frame(&ctx, 1.25, vec![Event::PointerMoved(p_delta)], &mut text, id);
        frame(&ctx, 1.30, vec![Event::PointerMoved(Pos2::new(p_delta.x + 1.0, p_delta.y))], &mut text, id);
        let sel = selection(&ctx, id).unwrap();
        assert_eq!((sel.start, sel.end), (6, 22), "drag should extend by whole words: {sel:?}");
        frame(&ctx, 1.40, vec![press(p_delta, false)], &mut text, id);
    }

    /// Text the app would put on the clipboard for the current label selection (what Ctrl+C copies).
    fn copied(ctx: &egui::Context, t: f64, two_labels: &mut dyn FnMut(&mut egui::Ui)) -> String {
        let raw = egui::RawInput { time: Some(t), screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(600.0, 200.0))), events: vec![Event::Copy], focused: true, ..Default::default() };
        let out = ctx.run_ui(raw, |ui| two_labels(ui));
        out.platform_output.commands.iter().find_map(|c| if let egui::OutputCommand::CopyText(t) = c { Some(t.clone()) } else { None }).unwrap_or_default()
    }

    #[test]
    fn word_drag_across_two_labels_and_back() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|s| s.interaction.select_word_on_press = true);
        let (l1, l2) = ("alpha beta gamma delta", "one two three four");
        let rects = std::cell::Cell::new([Rect::NOTHING; 2]);
        let mut draw = |ui: &mut egui::Ui| { let a = ui.label(l1).rect; let b = ui.label(l2).rect; rects.set([a, b]); };
        let frame = |ctx: &egui::Context, t: f64, events: Vec<Event>, draw: &mut dyn FnMut(&mut egui::Ui)| {
            let raw = egui::RawInput { time: Some(t), screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(600.0, 200.0))), events, focused: true, ..Default::default() };
            let _ = ctx.run_ui(raw, |ui| draw(ui));
        };
        frame(&ctx, 0.0, vec![], &mut draw);
        let font = ctx.global_style().text_styles[&egui::TextStyle::Body].clone();
        let at = |ctx: &egui::Context, r: Rect, text: &str, i: usize| {
            let g = ctx.fonts_mut(|f| f.layout_no_wrap(text.to_string(), font.clone(), egui::Color32::WHITE));
            Pos2::new(r.min.x + g.pos_from_cursor(CCursor::new(i)).min.x + 2.0, r.center().y)
        };
        let rects = rects.get();
        let (p_beta, p_three, p_delta) = (at(&ctx, rects[0], l1, 8), at(&ctx, rects[1], l2, 10), at(&ctx, rects[0], l1, 19));
        let press = |p: Pos2, down: bool| Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: down, modifiers: Default::default() };
        frame(&ctx, 1.00, vec![Event::PointerMoved(p_beta), press(p_beta, true)], &mut draw);
        frame(&ctx, 1.05, vec![press(p_beta, false)], &mut draw);
        frame(&ctx, 1.15, vec![press(p_beta, true)], &mut draw);
        assert_eq!(copied(&ctx, 1.16, &mut draw), "beta", "second press");
        // onto the second label, then back to the first
        frame(&ctx, 1.25, vec![Event::PointerMoved(p_three)], &mut draw);
        frame(&ctx, 1.30, vec![Event::PointerMoved(Pos2::new(p_three.x + 1.0, p_three.y))], &mut draw);
        let below = copied(&ctx, 1.31, &mut draw);
        println!("on the second label: {below:?}");
        frame(&ctx, 1.40, vec![Event::PointerMoved(p_delta)], &mut draw);
        frame(&ctx, 1.45, vec![Event::PointerMoved(Pos2::new(p_delta.x + 1.0, p_delta.y))], &mut draw);
        let back = copied(&ctx, 1.46, &mut draw);
        println!("back on the first label: {back:?}");
        assert_eq!(back, "beta gamma delta", "back on the first row the selection should be whole words again");
        assert_eq!(below, "beta gamma delta\none two three", "on the second row both ends should be whole words");
    }

    #[test]
    fn triple_press_keeps_the_whole_line_and_drag_extends_by_lines() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|s| s.interaction.select_word_on_press = true);
        let (l1, l2) = ("alpha beta gamma delta", "one two three four");
        let rects = std::cell::Cell::new([Rect::NOTHING; 2]);
        let mut draw = |ui: &mut egui::Ui| { let a = ui.label(l1).rect; let b = ui.label(l2).rect; rects.set([a, b]); };
        let frame = |ctx: &egui::Context, t: f64, events: Vec<Event>, draw: &mut dyn FnMut(&mut egui::Ui)| {
            let raw = egui::RawInput { time: Some(t), screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(600.0, 200.0))), events, focused: true, ..Default::default() };
            let _ = ctx.run_ui(raw, |ui| draw(ui));
        };
        frame(&ctx, 0.0, vec![], &mut draw);
        let font = ctx.global_style().text_styles[&egui::TextStyle::Body].clone();
        let rects = rects.get();
        let at = |ctx: &egui::Context, r: Rect, text: &str, i: usize| {
            let g = ctx.fonts_mut(|f| f.layout_no_wrap(text.to_string(), font.clone(), egui::Color32::WHITE));
            Pos2::new(r.min.x + g.pos_from_cursor(CCursor::new(i)).min.x + 2.0, r.center().y)
        };
        let (p_beta, p_three) = (at(&ctx, rects[0], l1, 8), at(&ctx, rects[1], l2, 10));
        let press = |p: Pos2, down: bool| Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: down, modifiers: Default::default() };
        frame(&ctx, 1.00, vec![Event::PointerMoved(p_beta), press(p_beta, true)], &mut draw);
        frame(&ctx, 1.04, vec![press(p_beta, false)], &mut draw);
        frame(&ctx, 1.10, vec![press(p_beta, true)], &mut draw);
        frame(&ctx, 1.14, vec![press(p_beta, false)], &mut draw);
        frame(&ctx, 1.20, vec![press(p_beta, true)], &mut draw);
        assert_eq!(copied(&ctx, 1.21, &mut draw), l1, "triple press selects the line");
        // the button is still down: the line must stay selected over the following frames
        for k in 0..4 { frame(&ctx, 1.25 + k as f64 * 0.02, vec![], &mut draw); }
        assert_eq!(copied(&ctx, 1.40, &mut draw), l1, "the line must not fall back to a word");
        frame(&ctx, 1.50, vec![Event::PointerMoved(p_three)], &mut draw);
        frame(&ctx, 1.55, vec![Event::PointerMoved(Pos2::new(p_three.x + 1.0, p_three.y))], &mut draw);
        assert_eq!(copied(&ctx, 1.56, &mut draw), format!("{l1}\n{l2}"), "dragging after a triple press extends by whole lines");
    }
}
