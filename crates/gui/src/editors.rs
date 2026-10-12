//! Modal dialogs for editing servers, users and passwords.

use eframe::egui::{self, Color32, RichText, Ui};
use manageriliki_core::{
    FlowControl, Parity, Role, SecretUpdate, SerialConfig, Server, ServerInput, SshAuth, SshConfig, User,
    VncConfig,
};

const BAUD_RATES: [u32; 10] = [1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 230400, 921600];

pub enum DialogResult {
    Open,
    Submit,
    Cancel,
}

/// Shows a modal with Save/Cancel buttons; Escape or clicking outside cancels.
fn modal(ctx: &egui::Context, id: &str, title: &str, error: &Option<String>, body: impl FnOnce(&mut Ui)) -> DialogResult {
    let mut result = DialogResult::Open;
    let response = egui::Modal::new(egui::Id::new(id)).show(ctx, |ui| {
        ui.set_width(440.0);
        ui.heading(title);
        ui.add_space(6.0);
        body(ui);
        if let Some(e) = error {
            ui.add_space(6.0);
            ui.colored_label(Color32::LIGHT_RED, e);
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button(RichText::new("Save").strong()).clicked() {
                result = DialogResult::Submit;
            }
            if ui.button("Cancel").clicked() {
                result = DialogResult::Cancel;
            }
        });
    });
    if response.should_close() && matches!(result, DialogResult::Open) {
        result = DialogResult::Cancel;
    }
    result
}

fn row(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.label(label);
    add(ui);
    ui.end_row();
}

fn text(ui: &mut Ui, value: &mut String) {
    ui.add(egui::TextEdit::singleline(value).desired_width(260.0));
}

#[derive(Clone, Copy, PartialEq)]
enum AuthKind {
    Agent,
    Password,
    Key,
}

pub struct ServerEditor {
    pub id: Option<i64>,
    pub error: Option<String>,
    name: String,
    group: String,
    notes: String,
    required_role: Role,
    ssh_enabled: bool,
    host: String,
    port: String,
    username: String,
    auth: AuthKind,
    key_path: String,
    secret: String,
    has_secret: bool,
    serial_enabled: bool,
    serial: SerialConfig,
    vnc_enabled: bool,
    vnc: VncConfig,
    vnc_secret: String,
    has_vnc_secret: bool,
}

impl ServerEditor {
    pub fn new(server: Option<&Server>) -> Self {
        let input = server.map(ServerInput::from_server).unwrap_or_default();
        let ssh = input.ssh.clone().unwrap_or_default();
        let (auth, key_path) = match &ssh.auth {
            SshAuth::Agent => (AuthKind::Agent, "~/.ssh/id_ed25519".to_string()),
            SshAuth::Password => (AuthKind::Password, "~/.ssh/id_ed25519".to_string()),
            SshAuth::Key { path } => (AuthKind::Key, path.clone()),
        };
        ServerEditor {
            id: server.map(|s| s.id),
            error: None,
            name: input.name,
            group: input.group,
            notes: input.notes,
            required_role: input.required_role,
            ssh_enabled: input.ssh.is_some(),
            host: ssh.host,
            port: ssh.port.to_string(),
            username: ssh.username,
            auth,
            key_path,
            secret: String::new(),
            has_secret: server.is_some_and(|s| s.has_secret),
            serial_enabled: input.serial.is_some(),
            serial: input.serial.unwrap_or_default(),
            vnc_enabled: input.vnc.is_some(),
            vnc: input.vnc.unwrap_or_default(),
            vnc_secret: String::new(),
            has_vnc_secret: server.is_some_and(|s| s.has_vnc_secret),
        }
    }

    pub fn to_input(&self) -> Result<ServerInput, String> {
        let ssh = if self.ssh_enabled {
            let port = self.port.trim().parse::<u16>().map_err(|_| "port must be 1-65535".to_string())?;
            let auth = match self.auth {
                AuthKind::Agent => SshAuth::Agent,
                AuthKind::Password => SshAuth::Password,
                AuthKind::Key => SshAuth::Key { path: self.key_path.clone() },
            };
            Some(SshConfig { host: self.host.clone(), port, username: self.username.clone(), auth })
        } else {
            None
        };
        Ok(ServerInput {
            name: self.name.clone(),
            group: self.group.clone(),
            notes: self.notes.clone(),
            ssh,
            serial: self.serial_enabled.then(|| self.serial.clone()),
            vnc: self.vnc_enabled.then(|| self.vnc.clone()),
            required_role: self.required_role,
            secret: if self.secret.is_empty() { SecretUpdate::Keep } else { SecretUpdate::Set(self.secret.clone()) },
            vnc_secret: if self.vnc_secret.is_empty() {
                SecretUpdate::Keep
            } else {
                SecretUpdate::Set(self.vnc_secret.clone())
            },
        })
    }

    pub fn show(&mut self, ctx: &egui::Context) -> DialogResult {
        let title = if self.id.is_some() { "Edit server" } else { "New server" };
        let error = self.error.clone();
        modal(ctx, "server-editor", title, &error, |ui| {
            egui::Grid::new("server-general").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                row(ui, "Name", |ui| text(ui, &mut self.name));
                row(ui, "Group", |ui| text(ui, &mut self.group));
                row(ui, "Notes", |ui| text(ui, &mut self.notes));
                row(ui, "Who may connect", |ui| {
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.required_role, Role::Operator, "operators & admins");
                        ui.radio_value(&mut self.required_role, Role::Admin, "admins only");
                    });
                });
            });

            ui.add_space(6.0);
            ui.checkbox(&mut self.ssh_enabled, RichText::new("SSH").strong());
            ui.add_enabled_ui(self.ssh_enabled, |ui| {
                egui::Grid::new("server-ssh").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    row(ui, "Host", |ui| text(ui, &mut self.host));
                    row(ui, "Port", |ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(70.0));
                    });
                    row(ui, "Username", |ui| text(ui, &mut self.username));
                    row(ui, "Authentication", |ui| {
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut self.auth, AuthKind::Agent, "ssh-agent");
                            ui.radio_value(&mut self.auth, AuthKind::Key, "key file");
                            ui.radio_value(&mut self.auth, AuthKind::Password, "password");
                        });
                    });
                    if self.auth == AuthKind::Key {
                        row(ui, "Key file", |ui| text(ui, &mut self.key_path));
                    }
                    if self.auth != AuthKind::Agent {
                        let label = if self.auth == AuthKind::Key { "Passphrase" } else { "Password" };
                        let hint = if self.has_secret { "stored — leave blank to keep" } else { "" };
                        row(ui, label, |ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.secret).password(true).hint_text(hint).desired_width(260.0));
                        });
                    }
                });
            });

            ui.add_space(6.0);
            ui.checkbox(&mut self.serial_enabled, RichText::new("Serial (RS232)").strong());
            ui.add_enabled_ui(self.serial_enabled, |ui| {
                let s = &mut self.serial;
                egui::Grid::new("server-serial").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    row(ui, "Device", |ui| {
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut s.device).desired_width(200.0));
                            ui.menu_button("Detect", |ui| {
                                match manageriliki_core::serial_ports() {
                                    Ok(ports) if !ports.is_empty() => {
                                        for p in ports {
                                            if ui.button(&p).clicked() {
                                                s.device = p;
                                            }
                                        }
                                    }
                                    Ok(_) => {
                                        ui.label("No serial ports found");
                                    }
                                    Err(e) => {
                                        ui.label(e.to_string());
                                    }
                                }
                            });
                        });
                    });
                    row(ui, "Baud", |ui| {
                        egui::ComboBox::from_id_salt("baud").selected_text(s.baud.to_string()).show_ui(ui, |ui| {
                            for b in BAUD_RATES {
                                ui.selectable_value(&mut s.baud, b, b.to_string());
                            }
                        });
                    });
                    row(ui, "Framing", |ui| {
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt("bits").width(50.0).selected_text(format!("{} bits", s.data_bits)).show_ui(ui, |ui| {
                                for b in [8u8, 7, 6, 5] {
                                    ui.selectable_value(&mut s.data_bits, b, b.to_string());
                                }
                            });
                            egui::ComboBox::from_id_salt("parity").width(70.0).selected_text(format!("{:?} parity", s.parity)).show_ui(ui, |ui| {
                                ui.selectable_value(&mut s.parity, Parity::None, "None");
                                ui.selectable_value(&mut s.parity, Parity::Odd, "Odd");
                                ui.selectable_value(&mut s.parity, Parity::Even, "Even");
                            });
                            egui::ComboBox::from_id_salt("stop").width(50.0).selected_text(format!("{} stop", s.stop_bits)).show_ui(ui, |ui| {
                                ui.selectable_value(&mut s.stop_bits, 1, "1");
                                ui.selectable_value(&mut s.stop_bits, 2, "2");
                            });
                        });
                    });
                    row(ui, "Flow control", |ui| {
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut s.flow_control, FlowControl::None, "none");
                            ui.radio_value(&mut s.flow_control, FlowControl::Software, "XON/XOFF");
                            ui.radio_value(&mut s.flow_control, FlowControl::Hardware, "RTS/CTS");
                        });
                    });
                });
            });

            ui.add_space(6.0);
            ui.checkbox(&mut self.vnc_enabled, RichText::new("VNC display").strong());
            ui.add_enabled_ui(self.vnc_enabled, |ui| {
                let v = &mut self.vnc;
                egui::Grid::new("server-vnc").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    row(ui, "Connection", |ui| {
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut v.via_ssh, false, "direct");
                            ui.add_enabled_ui(self.ssh_enabled, |ui| {
                                ui.radio_value(&mut v.via_ssh, true, "tunnel through SSH (encrypted)");
                            });
                        });
                    });
                    let hint = match (v.via_ssh, self.ssh_enabled) {
                        (true, _) => "127.0.0.1 (as seen from the server)",
                        (false, true) => "same as SSH host",
                        (false, false) => "",
                    };
                    row(ui, "Host", |ui| {
                        ui.add(egui::TextEdit::singleline(&mut v.host).hint_text(hint).desired_width(260.0));
                    });
                    row(ui, "Port", |ui| {
                        ui.horizontal(|ui| {
                            ui.add(egui::DragValue::new(&mut v.port).range(1..=65535));
                            ui.label(RichText::new(format!("display :{}", v.port.saturating_sub(5900))).weak());
                        });
                    });
                    row(ui, "Password", |ui| {
                        let hint = if self.has_vnc_secret { "stored — leave blank to keep" } else { "none" };
                        ui.add(egui::TextEdit::singleline(&mut self.vnc_secret).password(true).hint_text(hint).desired_width(260.0))
                            .on_hover_text("VNC passwords are limited to 8 characters by the protocol.");
                    });
                });
                if !v.via_ssh {
                    ui.label(
                        RichText::new("Direct VNC is unencrypted; fine on a trusted LAN, otherwise prefer the SSH tunnel.")
                            .weak()
                            .small(),
                    );
                }
            });
        })
    }
}

pub struct UserEditor {
    pub id: Option<i64>,
    pub error: Option<String>,
    pub username: String,
    pub password: String,
    pub role: Role,
    pub disabled: bool,
}

impl UserEditor {
    pub fn new(user: Option<&User>) -> Self {
        UserEditor {
            id: user.map(|u| u.id),
            error: None,
            username: user.map(|u| u.username.clone()).unwrap_or_default(),
            password: String::new(),
            role: user.map(|u| u.role).unwrap_or(Role::Operator),
            disabled: user.is_some_and(|u| u.disabled),
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> DialogResult {
        let title = match self.id {
            Some(_) => format!("Edit user {}", self.username),
            None => "New user".to_string(),
        };
        let error = self.error.clone();
        modal(ctx, "user-editor", &title, &error, |ui| {
            egui::Grid::new("user-form").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                if self.id.is_none() {
                    row(ui, "Username", |ui| text(ui, &mut self.username));
                    row(ui, "Password", |ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.password).password(true).desired_width(260.0));
                    });
                }
                row(ui, "Role", |ui| {
                    ui.vertical(|ui| {
                        ui.radio_value(&mut self.role, Role::Viewer, "viewer — sees servers and status");
                        ui.radio_value(&mut self.role, Role::Operator, "operator — can also connect and run commands");
                        ui.radio_value(&mut self.role, Role::Admin, "admin — can also manage servers and users");
                    });
                });
                if self.id.is_some() {
                    row(ui, "Account", |ui| {
                        ui.checkbox(&mut self.disabled, "disabled");
                    });
                }
            });
        })
    }
}

pub struct PasswordDialog {
    /// `None` changes the caller's own password.
    pub user_id: Option<i64>,
    target: String,
    pub old: String,
    pub new: String,
    pub confirm: String,
    pub error: Option<String>,
}

impl PasswordDialog {
    pub fn own() -> Self {
        PasswordDialog { user_id: None, target: String::new(), old: String::new(), new: String::new(), confirm: String::new(), error: None }
    }

    pub fn reset(user: &User) -> Self {
        PasswordDialog { user_id: Some(user.id), target: user.username.clone(), ..Self::own() }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> DialogResult {
        let title = match self.user_id {
            None => "Change my password".to_string(),
            Some(_) => format!("Reset password for {}", self.target),
        };
        let error = self.error.clone();
        modal(ctx, "password-dialog", &title, &error, |ui| {
            egui::Grid::new("password-form").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                let pw = |ui: &mut Ui, v: &mut String| {
                    ui.add(egui::TextEdit::singleline(v).password(true).desired_width(240.0));
                };
                if self.user_id.is_none() {
                    row(ui, "Current password", |ui| pw(ui, &mut self.old));
                }
                row(ui, "New password", |ui| pw(ui, &mut self.new));
                row(ui, "Confirm", |ui| pw(ui, &mut self.confirm));
            });
        })
    }
}
