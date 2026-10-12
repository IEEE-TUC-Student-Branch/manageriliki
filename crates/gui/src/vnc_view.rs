//! A VNC viewer window: renders the remote framebuffer as a texture and
//! forwards pointer and keyboard input.

use eframe::egui::{
    self, Color32, ColorImage, CornerRadius, CursorIcon, Event, EventFilter, Key, Modifiers,
    PointerButton, Rect, RichText, Sense, TextureHandle, TextureOptions, Ui, pos2,
};
use manageriliki_core::{VncSession, VncState, keysym};

pub struct VncView {
    pub id: u64,
    pub session: VncSession,
    pub open: bool,
    texture: Option<TextureHandle>,
    size_generation: u64,
    fit: bool,
    view_only: bool,
    last_pointer: Option<(u16, u16, u8)>,
    held_mods: Modifiers,
}

impl VncView {
    pub fn new(id: u64, session: VncSession) -> Self {
        VncView {
            id,
            session,
            open: true,
            texture: None,
            size_generation: u64::MAX,
            fit: true,
            view_only: false,
            last_pointer: None,
            held_mods: Modifiers::NONE,
        }
    }

    pub fn show(&mut self, ui: &mut Ui) {
        let state = self.session.state();
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.fit, true, "Fit");
            ui.selectable_value(&mut self.fit, false, "1:1");
            ui.separator();
            ui.checkbox(&mut self.view_only, "View only");
            let connected = state == VncState::Connected;
            if ui
                .add_enabled(connected && !self.view_only, egui::Button::new("Ctrl+Alt+Del"))
                .clicked()
            {
                self.session.ctrl_alt_del();
            }
            ui.separator();
            let (w, h) = self.session.with_framebuffer(|fb| (fb.width, fb.height));
            match &state {
                VncState::Connecting => {
                    ui.spinner();
                    ui.label("Connecting…");
                }
                VncState::Connected => {
                    ui.label(RichText::new(format!("{} · {w}×{h}", self.session.desktop_name())).weak());
                }
                VncState::Closed(None) => {
                    ui.colored_label(Color32::LIGHT_RED, "Disconnected");
                }
                VncState::Closed(Some(e)) => {
                    ui.colored_label(Color32::LIGHT_RED, e);
                }
            }
        });

        self.upload(ui.ctx());
        if let Some(text) = self.session.take_clipboard() {
            ui.ctx().copy_text(text);
        }
        let Some(texture) = self.texture.clone() else {
            ui.allocate_space(ui.available_size());
            return;
        };

        let fb_size = texture.size_vec2();
        if self.fit {
            let avail = ui.available_size();
            let scale = (avail.x / fb_size.x).min(avail.y / fb_size.y).max(0.05);
            self.display(ui, &texture, fb_size * scale, scale);
        } else {
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                self.display(ui, &texture, fb_size, 1.0);
            });
        }
    }

    /// Uploads the changed part of the framebuffer to the GPU texture.
    fn upload(&mut self, ctx: &egui::Context) {
        self.session.with_framebuffer(|fb| {
            if fb.width == 0 || fb.height == 0 {
                return;
            }
            let full = self.texture.is_none() || self.size_generation != fb.size_generation;
            let dirty = fb.take_dirty();
            if full {
                let image = ColorImage::from_rgba_unmultiplied([fb.width, fb.height], &fb.pixels);
                let name = format!("vnc-{}", self.id);
                self.texture = Some(ctx.load_texture(name, image, TextureOptions::LINEAR));
                self.size_generation = fb.size_generation;
            } else if let (Some(texture), Some((x, y, w, h))) = (&mut self.texture, dirty) {
                let mut rgba = Vec::with_capacity(w * h * 4);
                for row in y..y + h {
                    let start = (row * fb.width + x) * 4;
                    rgba.extend_from_slice(&fb.pixels[start..start + w * 4]);
                }
                let patch = ColorImage::from_rgba_unmultiplied([w, h], &rgba);
                texture.set_partial([x, y], patch, TextureOptions::LINEAR);
            }
        });
    }

    fn display(&mut self, ui: &mut Ui, texture: &TextureHandle, size: egui::Vec2, scale: f32) {
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let id = egui::Id::new(("vnc-view", self.id));
        let response = ui.interact(rect, id, Sense::click_and_drag());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Remote display"));
        ui.painter().image(
            texture.id(),
            rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );

        if response.clicked() || response.drag_started() {
            response.request_focus();
        }
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                id,
                EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
            )
        });
        let focused = response.has_focus();
        if focused {
            ui.painter().rect_stroke(
                rect,
                CornerRadius::ZERO,
                egui::Stroke::new(1.0, ui.visuals().selection.stroke.color),
                egui::StrokeKind::Outside,
            );
        }

        if self.view_only || self.session.state() != VncState::Connected {
            self.release_modifiers();
            return;
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
        }
        self.pointer(ui, rect, scale, response.hovered() || response.dragged());
        if focused {
            self.keyboard(ui);
        } else {
            self.release_modifiers();
        }
    }

    fn pointer(&mut self, ui: &Ui, rect: Rect, scale: f32, active: bool) {
        let (pos, buttons, wheel) = ui.input(|i| {
            let mut buttons = 0u8;
            for (button, bit) in [(PointerButton::Primary, 1), (PointerButton::Middle, 2), (PointerButton::Secondary, 4)] {
                if i.pointer.button_down(button) {
                    buttons |= bit;
                }
            }
            let wheel: f32 = i
                .events
                .iter()
                .filter_map(|e| match e {
                    Event::MouseWheel { delta, .. } => Some(delta.y),
                    _ => None,
                })
                .sum();
            (i.pointer.latest_pos(), buttons, wheel)
        });
        let Some(pos) = pos.filter(|_| active) else { return };
        let to_fb = |v: f32, max: f32| ((v / scale).floor().clamp(0.0, max - 1.0)) as u16;
        let size = rect.size() / scale;
        let x = to_fb(pos.x - rect.min.x, size.x);
        let y = to_fb(pos.y - rect.min.y, size.y);
        if !rect.contains(pos) && buttons == 0 {
            return;
        }
        if self.last_pointer != Some((x, y, buttons)) {
            self.session.pointer(x, y, buttons);
            self.last_pointer = Some((x, y, buttons));
        }
        if wheel.abs() > 0.0 && rect.contains(pos) {
            let bit = if wheel > 0.0 { 8 } else { 16 };
            self.session.pointer(x, y, buttons | bit);
            self.session.pointer(x, y, buttons);
        }
    }

    fn sync_modifiers(&mut self, now: Modifiers) {
        for (held, current, sym) in [
            (self.held_mods.shift, now.shift, keysym::SHIFT_L),
            (self.held_mods.ctrl, now.ctrl, keysym::CONTROL_L),
            (self.held_mods.alt, now.alt, keysym::ALT_L),
            (self.held_mods.mac_cmd, now.mac_cmd, keysym::SUPER_L),
        ] {
            if held != current {
                self.session.key(sym, current);
            }
        }
        self.held_mods = now;
    }

    fn release_modifiers(&mut self) {
        if self.held_mods != Modifiers::NONE {
            self.sync_modifiers(Modifiers::NONE);
        }
    }

    fn tap(&self, sym: u32) {
        self.session.key(sym, true);
        self.session.key(sym, false);
    }

    fn keyboard(&mut self, ui: &Ui) {
        let (events, mods) = ui.input(|i| (i.events.clone(), i.modifiers));
        self.sync_modifiers(mods);
        for event in events {
            match event {
                Event::Text(text) => {
                    for c in text.chars() {
                        self.tap(keysym::from_char(c));
                    }
                }
                // egui reports Ctrl+C/X/V as clipboard events instead of keys.
                Event::Copy => self.tap(keysym::from_char('c')),
                Event::Cut => self.tap(keysym::from_char('x')),
                Event::Paste(text) => {
                    self.session.set_clipboard(&text);
                    self.tap(keysym::from_char('v'));
                }
                Event::Key { key, pressed, modifiers, .. } => {
                    if let Some(sym) = special_keysym(key) {
                        self.session.key(sym, pressed);
                    } else if modifiers.ctrl || modifiers.alt {
                        // With Ctrl/Alt held no Text event arrives; send the base key.
                        let name = key.name();
                        if let [c] = name.as_bytes() {
                            self.session.key(keysym::from_char(c.to_ascii_lowercase() as char), pressed);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn special_keysym(key: Key) -> Option<u32> {
    use keysym::*;
    Some(match key {
        Key::Enter => RETURN,
        Key::Backspace => BACKSPACE,
        Key::Tab => TAB,
        Key::Escape => ESCAPE,
        Key::Delete => DELETE,
        Key::Insert => INSERT,
        Key::Home => HOME,
        Key::End => END,
        Key::PageUp => PAGE_UP,
        Key::PageDown => PAGE_DOWN,
        Key::ArrowLeft => LEFT,
        Key::ArrowRight => RIGHT,
        Key::ArrowUp => UP,
        Key::ArrowDown => DOWN,
        Key::F1 => F1,
        Key::F2 => F1 + 1,
        Key::F3 => F1 + 2,
        Key::F4 => F1 + 3,
        Key::F5 => F1 + 4,
        Key::F6 => F1 + 5,
        Key::F7 => F1 + 6,
        Key::F8 => F1 + 7,
        Key::F9 => F1 + 8,
        Key::F10 => F1 + 9,
        Key::F11 => F1 + 10,
        Key::F12 => F1 + 11,
        _ => return None,
    })
}
