use anyhow::Result;
use cairo::{Context, Format, ImageSurface, LinearGradient};

fn ease(t: f64) -> f64 {
    let t = t.clamp(0., 1.);
    if t < 0.5 {
        4. * t * t * t
    } else {
        1. - (-2. * t + 2.).powi(3) / 2.
    }
}

pub fn shell(
    width: i32,
    height: i32,
    color: [u8; 3],
    open: f64,
    level: f64,
    tick: f64,
) -> Result<Vec<u8>> {
    let mut surface = ImageSurface::create(Format::ARgb32, width, height)?;
    {
        let c = Context::new(&surface)?;
        let eased = ease(open);
        let rgb = color.map(|v| v as f64 / 255.);
        let h = height as f64;

        // Rock-solid fixed dimensions: the outer shell never wobbles or breathes
        let current_w = 88. + eased * 76.;
        let current_h = 14. + eased * 24.;
        let left = (width as f64 - current_w) / 2.;
        let top = h - current_h;

        // Fixed outer boundary (strictly inside carapace, zero rectangular shadow)
        carapace(&c, left, left + current_w, h, top);
        c.set_source_rgba(rgb[0], rgb[1], rgb[2], 0.18 + eased * 0.22);
        c.set_line_width(4.);
        c.stroke()?;

        // Inner solid dark fill
        let g = LinearGradient::new(0., top, 0., h);
        g.add_color_stop_rgba(0., 0.11, 0.14, 0.21, 0.99);
        g.add_color_stop_rgba(1., 0.02, 0.03, 0.05, 1.0);
        carapace(&c, left + 1.5, left + current_w - 1.5, h, top + 1.5);
        c.set_source(&g)?;
        c.fill_preserve()?;

        // Solid crisp border outline (constant width, never pulsing)
        c.set_source_rgba(rgb[0], rgb[1], rgb[2], 0.92);
        c.set_line_width(1.5);
        c.stroke()?;

        // Tech ribs (constant)
        c.set_source_rgba(rgb[0], rgb[1], rgb[2], 0.28);
        c.set_line_width(1.);
        for ratio in [0.28, 0.5, 0.72] {
            let x = left + current_w * ratio;
            c.move_to(x, top + 3.);
            c.line_to(left + current_w * (0.18 + ratio * 0.64), h - 1.);
            c.stroke()?;
        }

        // ONLY the interior bars move in response to your voice
        if eased > 0.08 {
            for i in -8i32..=8 {
                let x = width as f64 / 2. + i as f64 * 5.2;
                let distance = 1. - i.abs() as f64 / 9.2;
                let phase = i as f64 * 0.85;

                let bar = if level >= 0. {
                    if level > 0.02 {
                        // Voice detected: energetic dancing bars scaled dynamically to voice volume
                        let voice_jitter = (tick * 0.45 + phase).sin().abs() * 0.45
                            + (tick * 0.85 + phase * 1.8).sin().abs() * 0.35;
                        (3.0 + level * 25.0 * (0.35 + voice_jitter)) * eased * distance.powf(0.55)
                    } else {
                        // Silence while listening: gentle, subtle living wave so you know it's active
                        let idle_wave = (tick * 0.12 + phase * 0.45).sin().abs() * 2.0;
                        (2.5 + idle_wave) * eased * distance.powf(0.6)
                    }
                } else {
                    // Processing / transcribing state: smooth sweeping wave
                    (2.5 + 11.0 * ((tick * 0.22 - phase * 0.55).sin() * 0.5 + 0.5))
                        * eased
                        * distance
                };

                let alpha = (0.45 + 0.55 * distance) * eased;
                c.set_source_rgba(rgb[0], rgb[1], rgb[2], alpha);
                rounded(&c, x - 1.6, h - 3. - bar, 3.2, bar.max(1.5), 1.6);
                c.fill()?;
            }
        }

        // Central grip handle (constant, solid)
        c.set_source_rgba(0.95, 0.97, 1., 0.85);
        rounded(
            &c,
            width as f64 / 2. - 7.,
            h - current_h * 0.55,
            14.,
            2.4,
            1.2,
        );
        c.fill()?;
    }
    surface.flush();
    Ok(surface.data()?.to_vec())
}
fn carapace(c: &Context, left: f64, right: f64, bottom: f64, top: f64) {
    let mid = (left + right) / 2.;
    let span = right - left;
    c.new_sub_path();
    c.move_to(left, bottom);
    c.line_to(left + span * 0.08, top + (bottom - top) * 0.42);
    c.curve_to(left + span * 0.22, top, left + span * 0.36, top, mid, top);
    c.curve_to(
        right - span * 0.36,
        top,
        right - span * 0.22,
        top,
        right - span * 0.08,
        top + (bottom - top) * 0.42,
    );
    c.line_to(right, bottom);
    c.close_path();
}
fn rounded(c: &Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    c.new_sub_path();
    c.arc(x + w - r, y + r, r, -std::f64::consts::FRAC_PI_2, 0.);
    c.arc(x + w - r, y + h - r, r, 0., std::f64::consts::FRAC_PI_2);
    c.arc(
        x + r,
        y + h - r,
        r,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::PI,
    );
    c.arc(
        x + r,
        y + r,
        r,
        std::f64::consts::PI,
        std::f64::consts::PI * 1.5,
    );
    c.close_path();
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dock_keeps_transparent_corners_and_animated_shape() {
        let small = shell(176, 44, [55, 170, 130], 0., 0., 0.).unwrap();
        let expanded = shell(176, 44, [235, 65, 75], 1., 0.7, 3.).unwrap();
        assert_ne!(small, expanded);
        assert_eq!(&small[..176 * 24 * 4], &vec![0; 176 * 24 * 4]);
        assert_eq!(&expanded[..4], &[0, 0, 0, 0]);
    }
    #[test]
    fn ease_curve_is_monotonic() {
        assert_eq!(ease(0.), 0.);
        assert!((ease(1.) - 1.).abs() < 1e-9);
        for i in 1..=20 {
            assert!(ease(i as f64 / 20.) >= ease((i - 1) as f64 / 20.));
        }
    }
}
