use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Stroke, Vec2};

pub fn ease(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

pub fn pill_dimensions(width: f32, height: f32, open: f32) -> Rect {
    let eased = ease(open as f64) as f32;
    let pill_w = 88.0 + eased * 42.0; // 88px idle -> 130px recording
    let pill_h = 24.0 + eased * 4.0;  // 24px idle -> 28px recording
    let left = (width - pill_w) / 2.0;
    let top = (height - pill_h) / 2.0;
    Rect::from_min_size(Pos2::new(left, top), Vec2::new(pill_w, pill_h))
}

#[allow(clippy::too_many_arguments)]
pub fn draw_voice_pill(
    painter: &egui::Painter,
    width: f32,
    height: f32,
    color: Color32,
    open: f32,
    level: f32,
    tick: f32,
    _position: &str,
) {
    let eased = ease(open as f64) as f32;
    let rect = pill_dimensions(width, height, open);
    let radius = CornerRadius::same((rect.height() / 2.0).round() as u8);

    // 1. Sleek dark translucent pill background matching Zebar mantle (#181825)
    painter.rect_filled(rect, radius, Color32::from_rgb(24, 24, 37));

    // 2. Subtle crisp colored oval border
    let border_alpha = ((0.55 + eased * 0.40) * 255.0).clamp(0.0, 255.0) as u8;
    let border_color = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), border_alpha);
    painter.rect_stroke(
        rect.shrink(0.75),
        radius,
        Stroke::new(1.3, border_color),
        egui::StrokeKind::Inside,
    );

    // 3. Centered symmetric voice visualizer bars ONLY
    let count: i32 = if eased > 0.25 { 6 } else { 4 };
    let spacing = if eased > 0.25 { 7.5 } else { 7.0 };
    let center_x = rect.center().x;
    let center_y = rect.center().y;
    let max_bar_h = rect.height() - 8.0;

    for i in -count..=count {
        let x = center_x + (i as f32) * spacing;
        let dist = 1.0 - (i as f32).abs() / (count as f32 + 1.2);
        let phase = (i as f32) * 0.85;

        let bar_h = if level < 0.0 {
            // Processing sweeping wave
            let wave = (tick * 0.24 - phase * 0.65).sin() * 0.5 + 0.5;
            (4.0 + wave * (max_bar_h - 4.0)) * dist.powf(0.45)
        } else if eased > 0.05 {
            // Active recording voice equalizer
            if level > 0.015 {
                let jitter = (tick * 0.45 + phase).sin().abs() * 0.45
                    + (tick * 0.85 + phase * 1.8).sin().abs() * 0.35;
                (4.0 + level * (max_bar_h * 1.35) * (0.4 + jitter)) * dist.powf(0.5)
            } else {
                let gentle = (tick * 0.14 + phase * 0.5).sin().abs() * 3.0;
                (4.5 + gentle) * dist.powf(0.55)
            }
        } else {
            // Calm, clean idle voice indicator shape
            let base = 4.0 + 7.5 * dist.powf(1.4);
            let breathe = (tick * 0.06 + phase * 0.35).sin().abs() * 1.2;
            base + breathe
        };

        let clamped_h = bar_h.clamp(3.0, max_bar_h);
        let alpha = ((0.60 + 0.40 * dist) * (0.75 + 0.25 * eased) * 255.0).clamp(0.0, 255.0) as u8;
        let bar_color = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha);

        let bar_rect = Rect::from_center_size(Pos2::new(x, center_y), Vec2::new(3.0, clamped_h));
        painter.rect_filled(bar_rect, 1.5, bar_color);
    }
}
