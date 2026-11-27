//! Compact, wrapping mood picker arranged around the left side of an ellipse.
use eframe::egui::{self, Color32, FontId, Id, Key, Pos2, Rect, Sense, Stroke, Ui, Vec2};

#[derive(Clone, Copy)]
struct ArcPosition {
    target: i32,
    offset: f32,
    wheel: f32,
    selected: Option<usize>,
}

pub(crate) fn show(
    ui: &mut Ui,
    rect: Rect,
    items: &[(Option<&str>, &str)],
    selected: Option<usize>,
    enabled: bool,
) -> Option<usize> {
    if items.is_empty() || !ui.is_rect_visible(rect) {
        return None;
    }
    let count = items.len() as i32;
    let id = ui.id().with("wave-arc");
    let mut state = ui.data(|data| {
        data.get_temp::<ArcPosition>(id).unwrap_or(ArcPosition {
            target: selected.unwrap_or(0) as i32,
            offset: selected.unwrap_or(0) as f32,
            wheel: 0.0,
            selected,
        })
    });

    if state.selected != selected {
        state.selected = selected;
        if let Some(index) = selected {
            let distance = (index as i32 - state.target + count / 2).rem_euclid(count) - count / 2;
            state.target += distance;
        }
    }
    let visible = rect.intersect(ui.clip_rect());
    if enabled && ui.rect_contains_pointer(visible) {
        let scroll = ui.input_mut(|input| {
            let scroll = input.smooth_scroll_delta.y;
            input.smooth_scroll_delta.y = 0.0;
            scroll
        });
        if scroll != 0.0 {
            if (-scroll).signum() != state.wheel.signum() {
                state.wheel = 0.0;
            }
            state.wheel -= scroll;
            let steps = (state.wheel / 42.0).trunc() as i32;
            state.wheel -= steps as f32 * 42.0;
            state.target += steps.clamp(-4, 4);
        }
    }

    let center_x = rect.left() + 20.0;
    let center_y = rect.center().y;
    let y_radius = (rect.height() * 0.48 - 27.0).max(36.0);
    let x_radius = (rect.width() * 0.28).clamp(36.0, 78.0);
    let top = Pos2::new(center_x + x_radius * 0.9, rect.top() + 13.0);
    let bottom = Pos2::new(top.x, rect.bottom() - 13.0);
    let mut result = None;
    if arrow(ui, id.with("up"), top, true, enabled) {
        state.target -= 1;
    }
    if arrow(ui, id.with("down"), bottom, false, enabled) {
        state.target += 1;
    }
    let focused = |ui: &Ui| ui.memory(|memory| memory.focused());
    let focused_index = focused(ui)
        .and_then(|focus| (0..items.len()).find(|&index| focus == id.with(("mood", index))));
    if enabled && focused_index.is_some() {
        let step = ui.input_mut(|input| {
            let none = egui::Modifiers::NONE;
            if input.consume_key(none, Key::ArrowUp) {
                Some(-1)
            } else if input.consume_key(none, Key::ArrowDown) {
                Some(1)
            } else if input.consume_key(none, Key::Home) {
                Some(-state.target.rem_euclid(count))
            } else {
                None
            }
        });
        if let Some(step) = step {
            state.target += step;
            let index = state.target.rem_euclid(count) as usize;
            ui.memory_mut(|memory| memory.request_focus(id.with(("mood", index))));
        }
    }

    if (state.target as f32 - state.offset).abs() > 5.0 {
        let distance = (state.target as f32 - state.offset).signum();
        state.offset = state.target as f32 - 5.0 * distance;
    }
    let remaining = state.target as f32 - state.offset;
    if remaining.abs() < 0.015 {
        state.offset = state.target as f32;
    } else {
        let dt = ui.input(|input| input.stable_dt).min(0.05);
        state.offset += remaining * (1.0 - (-20.0 * dt).exp());
        ui.ctx().request_repaint();
    }

    // Use a local painter so the artwork never spills into the player or the next section.
    let painter = ui.painter().with_clip_rect(visible);
    let nearest = state.offset.round() as i32;
    let span = (if y_radius >= 168.0 { 3 } else { 2 }).min((count - 1) / 2);
    let angle_step = if span == 3 { 0.34 } else { 0.46 };
    let icon_base = (y_radius * 0.36).clamp(32.0, 58.0);
    for index in (nearest - span)..=(nearest + span) {
        let distance = index as f32 - state.offset;
        let angle = distance * angle_step;
        let emphasis = (1.0 - distance.abs() * 0.15).max(0.45);
        let size = icon_base * emphasis;
        let left = center_x + (1.0 - angle.cos()) * x_radius;
        let center = center_y + angle.sin() * y_radius;
        let item = index.rem_euclid(count) as usize;
        let (mood, text) = items[item];
        let active = selected == Some(item);
        let artwork =
            Rect::from_center_size(Pos2::new(left + icon_base * 0.5, center), Vec2::splat(size));
        let above = center_y + (angle - angle_step).sin() * y_radius;
        let below = center_y + (angle + angle_step).sin() * y_radius;
        let half_hit = (size * 0.5).max(15.0);
        let hit = Rect::from_min_max(
            Pos2::new(left, (center - half_hit).max((above + center) * 0.5)),
            Pos2::new(
                rect.right() - 6.0,
                (center + half_hit).min((below + center) * 0.5),
            ),
        )
        .intersect(visible);
        if hit.width() < 8.0 || hit.height() < 4.0 {
            continue;
        }
        let response = ui.interact(
            hit,
            id.with(("mood", item)),
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, enabled, active, text)
        });
        let ink = if active {
            Color32::from_rgb(255, 220, 60)
        } else if response.hovered() || response.has_focus() {
            Color32::WHITE
        } else {
            Color32::from_gray((155.0 + emphasis * 90.0) as u8)
        };
        if response.has_focus() {
            painter.rect_stroke(
                artwork.expand(2.0),
                8,
                Stroke::new(1.0_f32, ink),
                egui::StrokeKind::Inside,
            );
        }
        crate::wave_art::paint(&painter, artwork, mood, emphasis);
        let text_x = artwork.right() + 8.0;
        let mut job = egui::text::LayoutJob::simple(
            text.to_owned(),
            FontId::proportional(13.0 + 3.0 * emphasis),
            ink,
            (hit.right() - text_x).max(1.0),
        );
        job.wrap.max_rows = 1;
        let galley = painter.layout_job(job);
        painter.galley(
            Pos2::new(text_x, center - galley.size().y * 0.5),
            galley,
            ink,
        );
        if response.clicked() && enabled {
            state.target = index;
            result = Some(item);
        }
    }
    ui.data_mut(|data| data.insert_temp(id, state));
    result
}

fn arrow(ui: &Ui, id: Id, center: Pos2, up: bool, enabled: bool) -> bool {
    let rect = Rect::from_center_size(center, Vec2::new(36.0, 24.0));
    let response = ui.interact(
        rect,
        id,
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let label = if up {
        "Предыдущие настроения"
    } else {
        "Следующие настроения"
    };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
    let ink = Color32::from_gray(if response.hovered() || response.has_focus() {
        245
    } else {
        170
    });
    let y = if up { -2.0 } else { 2.0 };
    ui.painter().add(egui::Shape::line(
        vec![
            center + Vec2::new(-4.0, -y),
            center + Vec2::new(0.0, y),
            center + Vec2::new(4.0, -y),
        ],
        Stroke::new(1.5_f32, ink),
    ));
    enabled && response.on_hover_text(label).clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_browses_the_arc_without_changing_the_playing_mood() {
        let ctx = egui::Context::default();
        let items = [(None, "Обычная волна"), (Some("mood:calm"), "Спокойствие")];
        let frame = |pointer: Pos2, wheel: f32| {
            let mut result = None;
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 480.0))),
                    events: vec![egui::Event::PointerMoved(pointer)],
                    ..Default::default()
                },
                |ctx| {
                    ctx.input_mut(|input| input.smooth_scroll_delta.y = wheel);
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let rect = Rect::from_min_size(ui.min_rect().min, Vec2::new(250.0, 400.0));
                        assert_eq!(show(ui, rect, &items, Some(0), true), None);
                        let state: ArcPosition = ui
                            .data(|data| data.get_temp(ui.id().with("wave-arc")))
                            .unwrap();
                        result =
                            Some((state.target, ui.input(|input| input.smooth_scroll_delta.y)));
                    });
                },
            );
            result.unwrap()
        };
        let _ = frame(Pos2::new(100.0, 100.0), 0.0);
        assert_eq!(frame(Pos2::new(400.0, 100.0), -60.0), (0, -60.0));
        assert_eq!(frame(Pos2::new(100.0, 100.0), -60.0), (1, 0.0));
    }
}
