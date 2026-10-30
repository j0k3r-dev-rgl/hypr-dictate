use anyhow::Result;
use cairo::{Context, Format, ImageSurface, LinearGradient};

pub struct Modal<'a> {
    pub tab: usize,
    pub account: &'a str,
    pub model: &'a str,
    pub shortcut: &'a str,
    pub microphone: &'a str,
    pub monitor: &'a str,
    pub open: Option<usize>,
    pub options: &'a [String],
    pub selected: usize,
    pub message: &'a str,
    pub temperature: f64,
    pub prompt: &'a str,
    pub prompt_cursor: usize,
    pub prompt_focused: bool,
    pub base_url: &'a str,
    pub server_cursor: usize,
    pub server_focused: bool,
}

/// Row top offsets for the five selector fields in the modal. Kept as a
/// shared constant so `paint::modal` and `ui::modal_click`/`modal_scroll`
/// never drift apart on layout.
pub const ROW_TOP: [f64; 5] = [84., 150., 216., 282., 348.];
/// Height of the modal content needed to fit rows plus action buttons and status.
pub const MODAL_HEIGHT: i32 = 530;

pub const TAB_GENERAL_RECT: (f64, f64, f64, f64) = (26., 44., 100., 28.);
pub const TAB_IA_RECT: (f64, f64, f64, f64) = (134., 44., 140., 28.);
pub const TAB_SERVER_RECT: (f64, f64, f64, f64) = (282., 44., 118., 28.);
pub const SERVER_BOX: (f64, f64, f64, f64) = (26., 112., 488., 40.);
pub const TEMP_BOX: (f64, f64, f64, f64) = (26., 92., 488., 40.);
pub const PROMPT_BOX: (f64, f64, f64, f64) = (26., 156., 488., 238.);
pub const BTN_RESTORE_IA: (f64, f64, f64, f64) = (186., 412., 140., 38.);

pub fn wrap_prompt_lines(text: &str, max_chars: usize) -> Vec<(usize, String)> {
    let mut lines = Vec::new();
    let mut current_line = String::new();
    let mut line_start_char_idx = 0;
    let mut current_char_idx = 0;

    for word in text.split(' ') {
        let word_len = word.chars().count();
        let cur_len = current_line.chars().count();
        if !current_line.is_empty() && (cur_len + 1 + word_len) > max_chars {
            lines.push((line_start_char_idx, std::mem::take(&mut current_line)));
            line_start_char_idx = current_char_idx;
            current_line.push_str(word);
        } else {
            if !current_line.is_empty() {
                current_line.push(' ');
            }
            current_line.push_str(word);
        }
        current_char_idx += word_len + 1;
    }
    if !current_line.is_empty() {
        lines.push((line_start_char_idx, current_line));
    }
    if lines.is_empty() {
        lines.push((0, String::new()));
    }
    lines
}

fn draw_chevron(
    c: &Context,
    cx: f64,
    cy: f64,
    size: f64,
    up: bool,
    color: [f64; 4],
) {
    c.set_source_rgba(color[0], color[1], color[2], color[3]);
    c.set_line_width(2.0);
    c.set_line_cap(cairo::LineCap::Round);
    c.set_line_join(cairo::LineJoin::Round);
    c.new_sub_path();
    if up {
        c.move_to(cx - size, cy + size * 0.4);
        c.line_to(cx, cy - size * 0.4);
        c.line_to(cx + size, cy + size * 0.4);
    } else {
        c.move_to(cx - size, cy - size * 0.4);
        c.line_to(cx, cy + size * 0.4);
        c.line_to(cx + size, cy - size * 0.4);
    }
    let _ = c.stroke();
}

fn draw_close_cross(c: &Context, cx: f64, cy: f64, size: f64, color: [f64; 4]) {
    c.set_source_rgba(color[0], color[1], color[2], color[3]);
    c.set_line_width(1.8);
    c.set_line_cap(cairo::LineCap::Round);
    c.new_sub_path();
    c.move_to(cx - size, cy - size);
    c.line_to(cx + size, cy + size);
    c.move_to(cx + size, cy - size);
    c.line_to(cx - size, cy + size);
    let _ = c.stroke();
}

/// Smooth ease-in-out curve used for the dock expand/collapse animation so it
/// decelerates near both ends instead of moving at a constant rate.
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
                    (2.5 + 11.0 * ((tick * 0.22 - phase * 0.55).sin() * 0.5 + 0.5)) * eased * distance
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
pub fn modal(width: i32, height: i32, color: [u8; 3], m: &Modal<'_>) -> Result<Vec<u8>> {
    let mut surface = ImageSurface::create(Format::ARgb32, width, height)?;
    {
        let c = Context::new(&surface)?;
        let rgb = color.map(|v| v as f64 / 255.);
        let w = width as f64;
        let h = height as f64;
        let g = LinearGradient::new(0., 0., 0., h);
        // Opaque solid background so underlying windows/terminals never shine through
        g.add_color_stop_rgba(0., 0.08, 0.10, 0.15, 1.0);
        g.add_color_stop_rgba(1., 0.025, 0.035, 0.055, 1.0);
        rounded(&c, 2., 2., w - 4., h - 4., 20.);
        c.set_source(&g)?;
        c.fill_preserve()?;
        // Subtle top highlight for depth, then the accent-colored outline.
        let edge = LinearGradient::new(0., 0., 0., 60.);
        edge.add_color_stop_rgba(0., 1., 1., 1., 0.06);
        edge.add_color_stop_rgba(1., 1., 1., 1., 0.);
        c.set_source(&edge)?;
        c.fill_preserve()?;
        c.set_source_rgba(rgb[0], rgb[1], rgb[2], 0.8);
        c.set_line_width(1.6);
        c.stroke()?;
        c.select_font_face(
            "sans-serif",
            cairo::FontSlant::Normal,
            cairo::FontWeight::Bold,
        );
        c.set_font_size(15.);
        c.set_source_rgb(0.94, 0.96, 1.);
        c.move_to(26., 30.);
        c.show_text("Configuración de dictado")?;

        // Reload / Reset button in header
        rounded(&c, w - 192., 15., 90., 24., 6.);
        c.set_source_rgba(0.18, 0.22, 0.32, 0.85);
        c.fill_preserve()?;
        c.set_source_rgba(rgb[0], rgb[1], rgb[2], 0.45);
        c.set_line_width(1.0);
        c.stroke()?;
        c.set_font_size(11.);
        c.set_source_rgb(0.88, 0.92, 0.99);
        c.move_to(w - 182., 31.);
        c.show_text("⟳ Reiniciar")?;

        c.set_font_size(12.);
        c.set_source_rgb(0.6, 0.65, 0.76);
        c.move_to(w - 88., 28.);
        c.show_text("Cerrar")?;
        draw_close_cross(&c, w - 38., 24., 4.5, [0.6, 0.65, 0.76, 0.9]);

        // Tabs: [ General ] and [ Parámetros e IA ]
        for (tab_idx, (label, (tx, ty, tw, th))) in [
            ("General", TAB_GENERAL_RECT),
            ("Parámetros e IA", TAB_IA_RECT),
            ("Servidor", TAB_SERVER_RECT),
        ]
        .into_iter()
        .enumerate()
        {
            let is_active = m.tab == tab_idx;
            rounded(&c, tx, ty, tw, th, 7.);
            if is_active {
                c.set_source_rgba(rgb[0], rgb[1], rgb[2], 0.25);
                c.fill_preserve()?;
                c.set_source_rgba(rgb[0], rgb[1], rgb[2], 0.95);
                c.set_line_width(1.2);
                c.stroke()?;
                c.set_source_rgb(0.96, 0.98, 1.);
            } else {
                c.set_source_rgba(0.12, 0.15, 0.22, 0.7);
                c.fill_preserve()?;
                c.set_source_rgba(0.25, 0.30, 0.42, 0.6);
                c.set_line_width(1.0);
                c.stroke()?;
                c.set_source_rgb(0.62, 0.68, 0.80);
            }
            c.set_font_size(12.);
            c.move_to(tx + 12., ty + 19.);
            c.show_text(label)?;
        }

        let actions_y = 412.;

        if m.tab == 0 {
            for (idx, (label, value, y)) in [
                ("Cuenta / ruta", m.account, ROW_TOP[0]),
                ("Modelo (rueda para recorrer)", m.model, ROW_TOP[1]),
                ("Atajo de mantener para hablar", m.shortcut, ROW_TOP[2]),
                ("Micrófono (rueda para recorrer)", m.microphone, ROW_TOP[3]),
                ("Pantalla (rueda para recorrer)", m.monitor, ROW_TOP[4]),
            ]
            .into_iter()
            .enumerate()
            {
                c.set_source_rgb(0.68, 0.73, 0.84);
                c.move_to(26., y);
                c.show_text(label)?;
                c.set_source_rgba(0.12, 0.15, 0.21, 0.96);
                rounded(&c, 26., y + 8., w - 52., 38., 9.);
                c.fill_preserve()?;
                let active = m.open == Some(idx);
                c.set_source_rgba(rgb[0], rgb[1], rgb[2], if active { 0.95 } else { 0.32 });
                c.set_line_width(1.2);
                c.stroke()?;
                c.set_source_rgb(0.93, 0.95, 0.99);
                c.move_to(38., y + 32.);
                c.show_text(&value.chars().take(54).collect::<String>())?;
                draw_chevron(
                    &c,
                    w - 48.,
                    y + 27.,
                    5.0,
                    active,
                    [rgb[0], rgb[1], rgb[2], if active { 0.95 } else { 0.6 }],
                );
            }

            c.set_source_rgb(rgb[0], rgb[1], rgb[2]);
            rounded(&c, 26., actions_y, 180., 38., 9.);
            c.fill()?;
            c.set_source_rgb(0.04, 0.05, 0.08);
            c.move_to(56., actions_y + 24.);
            c.show_text("Guardar cambios")?;

            c.set_source_rgba(0.15, 0.18, 0.26, 0.95);
            rounded(&c, 216., actions_y, 110., 38., 9.);
            c.fill()?;
            c.set_source_rgb(0.87, 0.9, 0.96);
            c.move_to(248., actions_y + 24.);
            c.show_text("Cerrar")?;
        } else if m.tab == 2 {
            c.set_source_rgb(0.68, 0.73, 0.84);
            c.move_to(26., 96.);
            c.show_text("URL base de CLIProxyAPI")?;
            rounded(
                &c,
                SERVER_BOX.0,
                SERVER_BOX.1,
                SERVER_BOX.2,
                SERVER_BOX.3,
                9.,
            );
            c.set_source_rgba(0.09, 0.11, 0.16, 0.98);
            c.fill_preserve()?;
            c.set_source_rgba(
                rgb[0],
                rgb[1],
                rgb[2],
                if m.server_focused { 0.95 } else { 0.32 },
            );
            c.set_line_width(1.2);
            c.stroke()?;
            c.save()?;
            c.rectangle(
                SERVER_BOX.0 + 8.,
                SERVER_BOX.1 + 4.,
                SERVER_BOX.2 - 16.,
                SERVER_BOX.3 - 8.,
            );
            c.clip();
            c.select_font_face(
                "monospace",
                cairo::FontSlant::Normal,
                cairo::FontWeight::Normal,
            );
            c.set_font_size(12.);
            let before: String = m.base_url.chars().take(m.server_cursor).collect();
            let cursor_width = c.text_extents(&before)?.x_advance();
            let offset = if m.server_focused {
                (cursor_width - SERVER_BOX.2 + 32.).max(0.)
            } else {
                0.
            };
            c.set_source_rgb(0.93, 0.95, 0.99);
            c.move_to(SERVER_BOX.0 + 12. - offset, SERVER_BOX.1 + 25.);
            c.show_text(m.base_url)?;
            if m.server_focused {
                let x = SERVER_BOX.0 + 12. + cursor_width - offset;
                c.set_source_rgb(rgb[0], rgb[1], rgb[2]);
                c.move_to(x, SERVER_BOX.1 + 10.);
                c.line_to(x, SERVER_BOX.1 + 29.);
                c.stroke()?;
            }
            c.restore()?;
            c.select_font_face(
                "sans-serif",
                cairo::FontSlant::Normal,
                cairo::FontWeight::Bold,
            );
            c.set_font_size(12.);
            c.set_source_rgb(0.68, 0.73, 0.84);
            for (y, line) in [
                (180., "HTTP o HTTPS; sin /v1, credenciales ni parámetros."),
                (204., "Guardar servidor recarga el catálogo sin reiniciar."),
                (228., "No guarda otros cambios pendientes del modal."),
                (252., "Después elige un modelo compatible en General."),
            ] {
                c.move_to(26., y);
                c.show_text(line)?;
            }
            c.set_source_rgb(rgb[0], rgb[1], rgb[2]);
            rounded(&c, 26., actions_y, 180., 38., 9.);
            c.fill()?;
            c.set_source_rgb(0.04, 0.05, 0.08);
            c.move_to(50., actions_y + 24.);
            c.show_text("Guardar servidor")?;
            c.set_source_rgba(0.15, 0.18, 0.26, 0.95);
            rounded(&c, 216., actions_y, 110., 38., 9.);
            c.fill()?;
            c.set_source_rgb(0.87, 0.9, 0.96);
            c.move_to(248., actions_y + 24.);
            c.show_text("Cerrar")?;
        } else {
            // Tab 1: Parámetros e IA
            c.set_source_rgb(0.68, 0.73, 0.84);
            c.move_to(26., 84.);
            c.show_text("Temperatura del modelo (rueda del ratón para variar de 0.0 a 1.0)")?;

            rounded(&c, TEMP_BOX.0, TEMP_BOX.1, TEMP_BOX.2, TEMP_BOX.3, 9.);
            c.set_source_rgba(0.12, 0.15, 0.21, 0.96);
            c.fill_preserve()?;
            c.set_source_rgba(rgb[0], rgb[1], rgb[2], 0.35);
            c.set_line_width(1.2);
            c.stroke()?;

            let fill_w = (TEMP_BOX.2 - 4.) * m.temperature.clamp(0.0, 1.0);
            if fill_w > 2. {
                rounded(&c, TEMP_BOX.0 + 2., TEMP_BOX.1 + 2., fill_w, TEMP_BOX.3 - 4., 7.);
                c.set_source_rgba(rgb[0], rgb[1], rgb[2], 0.28);
                c.fill()?;
            }

            let desc = if m.temperature <= 0.10 {
                "Determinista / Muy estricto"
            } else if m.temperature <= 0.35 {
                "Equilibrado (recomendado)"
            } else {
                "Creativo / Mayor variación"
            };
            c.set_source_rgb(0.93, 0.96, 1.);
            c.move_to(40., TEMP_BOX.1 + 25.);
            c.show_text(&format!("{:.2}  ·  {}", m.temperature, desc))?;

            c.set_source_rgb(0.68, 0.73, 0.84);
            c.move_to(26., 148.);
            if m.prompt_focused {
                c.show_text("Prompt del sistema (escribe directamente para editar)")?;
            } else {
                c.show_text("Prompt del sistema (haz clic adentro para escribir y editar)")?;
            }

            rounded(&c, PROMPT_BOX.0, PROMPT_BOX.1, PROMPT_BOX.2, PROMPT_BOX.3, 10.);
            c.set_source_rgba(0.09, 0.11, 0.16, 0.98);
            c.fill_preserve()?;
            if m.prompt_focused {
                c.set_source_rgba(rgb[0], rgb[1], rgb[2], 0.95);
                c.set_line_width(1.5);
            } else {
                c.set_source_rgba(rgb[0], rgb[1], rgb[2], 0.32);
                c.set_line_width(1.2);
            }
            c.stroke()?;

            c.save()?;
            c.rectangle(
                PROMPT_BOX.0 + 8.,
                PROMPT_BOX.1 + 8.,
                PROMPT_BOX.2 - 16.,
                PROMPT_BOX.3 - 16.,
            );
            c.clip();

            c.select_font_face(
                "monospace",
                cairo::FontSlant::Normal,
                cairo::FontWeight::Normal,
            );
            c.set_font_size(11.0);
            c.set_source_rgb(0.90, 0.94, 0.99);

            let lines = wrap_prompt_lines(m.prompt, 54);
            let mut cursor_drawn = false;

            for (line_idx, (start_char_idx, line_str)) in lines.iter().enumerate() {
                let y = PROMPT_BOX.1 + 24. + line_idx as f64 * 18.;
                c.move_to(PROMPT_BOX.0 + 12., y);
                c.show_text(line_str)?;

                if m.prompt_focused && !cursor_drawn {
                    let line_char_count = line_str.chars().count();
                    let is_cursor_in_line = if line_idx + 1 == lines.len() {
                        m.prompt_cursor >= *start_char_idx
                    } else {
                        m.prompt_cursor >= *start_char_idx
                            && m.prompt_cursor < *start_char_idx + line_char_count
                    };

                    if is_cursor_in_line {
                        let offset_in_line = m
                            .prompt_cursor
                            .saturating_sub(*start_char_idx)
                            .min(line_char_count);
                        let sub: String = line_str.chars().take(offset_in_line).collect();
                        let extents = c.text_extents(&sub)?;
                        let cursor_x = PROMPT_BOX.0 + 12. + extents.x_advance();
                        c.set_source_rgba(rgb[0], rgb[1], rgb[2], 0.95);
                        c.set_line_width(1.8);
                        c.move_to(cursor_x, y - 12.);
                        c.line_to(cursor_x, y + 2.);
                        c.stroke()?;
                        c.set_source_rgb(0.90, 0.94, 0.99);
                        cursor_drawn = true;
                    }
                }
            }
            c.restore()?;
            c.select_font_face(
                "sans-serif",
                cairo::FontSlant::Normal,
                cairo::FontWeight::Bold,
            );

            c.set_source_rgb(rgb[0], rgb[1], rgb[2]);
            rounded(&c, 26., actions_y, 150., 38., 9.);
            c.fill()?;
            c.set_source_rgb(0.04, 0.05, 0.08);
            c.move_to(44., actions_y + 24.);
            c.show_text("Guardar cambios")?;

            c.set_source_rgba(0.18, 0.22, 0.32, 0.95);
            rounded(
                &c,
                BTN_RESTORE_IA.0,
                BTN_RESTORE_IA.1,
                BTN_RESTORE_IA.2,
                BTN_RESTORE_IA.3,
                9.,
            );
            c.fill_preserve()?;
            c.set_source_rgba(rgb[0], rgb[1], rgb[2], 0.45);
            c.set_line_width(1.0);
            c.stroke()?;
            c.set_source_rgb(0.92, 0.95, 1.);
            c.move_to(206., actions_y + 24.);
            c.show_text("Restaurar base")?;

            c.set_source_rgba(0.15, 0.18, 0.26, 0.95);
            rounded(&c, 336., actions_y, 90., 38., 9.);
            c.fill()?;
            c.set_source_rgb(0.87, 0.9, 0.96);
            c.move_to(360., actions_y + 24.);
            c.show_text("Cerrar")?;
        }

        if !m.message.is_empty() {
            let is_success = m.message.contains("guardados") || m.message.contains("restaurado");
            let dot_rgb = if is_success {
                [0.22, 0.78, 0.52]
            } else {
                [0.45, 0.65, 0.95]
            };
            c.set_source_rgba(dot_rgb[0], dot_rgb[1], dot_rgb[2], 0.12);
            rounded(&c, 26., actions_y + 46., w - 52., 26., 6.);
            c.fill()?;
            c.set_source_rgb(dot_rgb[0], dot_rgb[1], dot_rgb[2]);
            c.arc(38., actions_y + 59., 3.5, 0., std::f64::consts::TAU);
            c.fill()?;
            c.set_font_size(11.5);
            c.set_source_rgb(0.85, 0.89, 0.96);
            c.move_to(48., actions_y + 63.);
            c.show_text(&m.message.chars().take(62).collect::<String>())?;
        }

        if m.tab == 0 && let Some(idx) = m.open {
            let visible = m.options.len().min(6);
            let h_list = visible as f64 * 30. + 10.;
            let base_y = if idx == 4 {
                ROW_TOP[idx] - h_list + 6.
            } else {
                ROW_TOP[idx] + 50.
            };
            let start = m
                .selected
                .saturating_sub(2)
                .min(m.options.len().saturating_sub(visible));
            c.set_source_rgba(0.065, 0.085, 0.13, 0.99);
            rounded(&c, 26., base_y, w - 52., h_list, 10.);
            c.fill_preserve()?;
            c.set_source_rgba(rgb[0], rgb[1], rgb[2], 0.9);
            c.stroke()?;
            for (row, option) in m.options.iter().skip(start).take(visible).enumerate() {
                let y = base_y + 5. + row as f64 * 30.;
                if start + row == m.selected {
                    c.set_source_rgba(rgb[0], rgb[1], rgb[2], 0.25);
                    rounded(&c, 32., y, w - 64., 26., 7.);
                    c.fill()?;
                }
                c.set_source_rgb(0.92, 0.95, 0.99);
                c.move_to(42., y + 18.);
                c.show_text(&option.chars().take(54).collect::<String>())?;
            }
        }
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
    fn compact_carapace_grows_smoothly_and_modal_renders_selects() {
        let small = shell(176, 44, [55, 170, 130], 0., 0., 0.).unwrap();
        let expanded = shell(176, 44, [235, 65, 75], 1., 0.7, 3.).unwrap();
        assert_ne!(small, expanded);
        assert_eq!(&small[..(176 * 24 * 4)], &vec![0; 176 * 24 * 4][..]);
        // Corners must be 100% transparent: zero rectangular box / shadow artifacts
        assert_eq!(&expanded[0..4], &[0, 0, 0, 0]);

        let options = vec!["cic".into(), "j0k3r".into()];
        let mut m = Modal {
            tab: 0,
            account: "cic",
            model: "cic/gemini-3.5-flash-lite",
            shortcut: "Sin atajo",
            microphone: "Automático",
            monitor: "Automática",
            open: Some(0),
            options: &options,
            selected: 0,
            message: "Listo",
            temperature: 0.2,
            prompt: "Prompt",
            prompt_cursor: 6,
            prompt_focused: false,
            base_url: "http://127.0.0.1:8317",
            server_cursor: 21,
            server_focused: false,
        };
        for tab in 0..3 {
            m.tab = tab;
            m.server_focused = tab == 2;
            assert_eq!(
                modal(540, MODAL_HEIGHT, [55, 170, 130], &m).unwrap().len(),
                540 * MODAL_HEIGHT as usize * 4
            );
        }
    }
    #[test]
    fn ease_curve_starts_at_zero_ends_at_one_and_is_monotonic() {
        assert_eq!(ease(0.), 0.);
        assert!((ease(1.) - 1.).abs() < 1e-9);
        let mut prev = -1.;
        for i in 0..=20 {
            let t = i as f64 / 20.;
            let v = ease(t);
            assert!(v >= prev);
            prev = v;
        }
    }
}
