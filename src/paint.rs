use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

pub fn ease(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

pub fn carapace_points(left: f32, right: f32, flat_y: f32, arch_y: f32) -> Vec<Pos2> {
    let span = right - left;
    let mid = (left + right) / 2.0;
    let mut points = Vec::with_capacity(26);

    points.push(Pos2::new(left, flat_y));
    points.push(Pos2::new(left + span * 0.08, arch_y + (flat_y - arch_y) * 0.42));

    let p0 = Pos2::new(left + span * 0.08, arch_y + (flat_y - arch_y) * 0.42);
    let p1 = Pos2::new(left + span * 0.22, arch_y);
    let p2 = Pos2::new(left + span * 0.36, arch_y);
    let p3 = Pos2::new(mid, arch_y);
    for step in 1..=8 {
        let t = step as f32 / 8.0;
        let it = 1.0 - t;
        let x = it * it * it * p0.x + 3.0 * it * it * t * p1.x + 3.0 * it * t * t * p2.x + t * t * t * p3.x;
        let y = it * it * it * p0.y + 3.0 * it * it * t * p1.y + 3.0 * it * t * t * p2.y + t * t * t * p3.y;
        points.push(Pos2::new(x, y));
    }

    let p0 = Pos2::new(mid, arch_y);
    let p1 = Pos2::new(right - span * 0.36, arch_y);
    let p2 = Pos2::new(right - span * 0.22, arch_y);
    let p3 = Pos2::new(right - span * 0.08, arch_y + (flat_y - arch_y) * 0.42);
    for step in 1..=8 {
        let t = step as f32 / 8.0;
        let it = 1.0 - t;
        let x = it * it * it * p0.x + 3.0 * it * it * t * p1.x + 3.0 * it * t * t * p2.x + t * t * t * p3.x;
        let y = it * it * it * p0.y + 3.0 * it * it * t * p1.y + 3.0 * it * t * t * p2.y + t * t * t * p3.y;
        points.push(Pos2::new(x, y));
    }

    points.push(Pos2::new(right, flat_y));
    points
}

#[allow(clippy::too_many_arguments)]
pub fn draw_carapace(
    painter: &egui::Painter,
    width: f32,
    height: f32,
    color: Color32,
    open: f32,
    level: f32,
    tick: f32,
    position: &str,
) {
    let eased = ease(open as f64) as f32;
    let current_w = 88.0 + eased * 76.0;
    let current_h = 14.0 + eased * 24.0;
    let left = (width - current_w) / 2.0;

    let is_top = position == "Arriba";
    let (flat_y, arch_y) = if is_top {
        (0.0, current_h)
    } else {
        (height, height - current_h)
    };

    let points = carapace_points(left, left + current_w, flat_y, arch_y);

    // 1. Soft outer boundary glow (open curve, no top flat line)
    let glow_alpha = ((0.18 + eased * 0.22) * 255.0).clamp(0.0, 255.0) as u8;
    let glow_color = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), glow_alpha);
    painter.add(egui::Shape::line(points.clone(), Stroke::new(3.5, glow_color)));

    // 2. Inner dark fill
    let inner_flat = if is_top { flat_y + 1.0 } else { flat_y - 1.0 };
    let inner_arch = if is_top { arch_y - 1.5 } else { arch_y + 1.5 };
    let inner_points = carapace_points(left + 1.5, left + current_w - 1.5, inner_flat, inner_arch);
    painter.add(egui::Shape::convex_polygon(inner_points, Color32::from_rgb(10, 14, 22), Stroke::NONE));

    // 3. Crisp outline (open curve along the arch only, no flat edge line)
    let border_color = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 235);
    painter.add(egui::Shape::line(points, Stroke::new(1.5, border_color)));

    // 4. Interior voice bars
    if eased > 0.08 {
        for i in -8..=8 {
            let x = width / 2.0 + (i as f32) * 5.2;
            let dist = 1.0 - (i as f32).abs() / 9.2;
            let phase = (i as f32) * 0.85;

            let bar = if level >= 0.0 {
                if level > 0.02 {
                    let voice_jitter = (tick * 0.45 + phase).sin().abs() * 0.45
                        + (tick * 0.85 + phase * 1.8).sin().abs() * 0.35;
                    (3.0 + level * 25.0 * (0.35 + voice_jitter)) * eased * dist.powf(0.55)
                } else {
                    let idle_wave = (tick * 0.12 + phase * 0.45).sin().abs() * 2.0;
                    (2.5 + idle_wave) * eased * dist.powf(0.6)
                }
            } else {
                // Processing sweeping wave
                (2.5 + 11.0 * ((tick * 0.22 - phase * 0.55).sin() * 0.5 + 0.5)) * eased * dist
            };

            let alpha = ((0.45 + 0.55 * dist) * eased * 255.0).clamp(0.0, 255.0) as u8;
            let bar_color = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha);
            let bar_h = bar.max(1.5);
            let bar_rect = if is_top {
                Rect::from_min_size(Pos2::new(x - 1.6, flat_y + 3.0), Vec2::new(3.2, bar_h))
            } else {
                Rect::from_min_size(Pos2::new(x - 1.6, flat_y - 3.0 - bar_h), Vec2::new(3.2, bar_h))
            };
            painter.rect_filled(bar_rect, 1.6, bar_color);
        }
    }

    // 5. Subtle central grip handle colored with state accent (no bright white line)
    let handle_y = if is_top {
        current_h * 0.52 - 1.2
    } else {
        flat_y - current_h * 0.52
    };
    let handle_color = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 190);
    painter.rect_filled(
        Rect::from_min_size(Pos2::new(width / 2.0 - 7.0, handle_y), Vec2::new(14.0, 2.4)),
        1.2,
        handle_color,
    );
}
