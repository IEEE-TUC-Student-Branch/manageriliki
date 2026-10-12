//! Headless UI tests. Set `MGR_SCREENSHOT_DIR` to also save rendered PNGs, and
//! `MGR_TEST_SSH=host:port:user:keyfile` to exercise a real SSH session.

use std::time::Duration;

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use manageriliki_core::{Manager, ServerInput, SshAuth, SshConfig};

use super::GuiApp;

fn screenshot(harness: &mut Harness<'_, GuiApp>, name: &str) {
    let Some(dir) = std::env::var_os("MGR_SCREENSHOT_DIR") else { return };
    match harness.render() {
        Ok(image) => image.save(std::path::Path::new(&dir).join(format!("{name}.png"))).unwrap(),
        Err(e) => eprintln!("render failed (no GPU adapter?): {e}"),
    }
}

fn type_into(harness: &mut Harness<'_, GuiApp>, label: &str, text: &str) {
    harness.get_by_label(label).focus();
    harness.run();
    harness.get_by_label(label).type_text(text);
    harness.run();
}

/// Runs frames until `done` holds or the timeout expires (for background work).
fn run_until(harness: &mut Harness<'_, GuiApp>, timeout: Duration, mut done: impl FnMut(&GuiApp) -> bool) {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout && !done(harness.state()) {
        harness.step();
        std::thread::sleep(Duration::from_millis(30));
    }
    harness.step();
}

#[test]
fn setup_then_manage_servers() {
    let dir = std::env::temp_dir().join(format!("mgr-gui-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mgr = Manager::open(&dir).unwrap();

    let app_mgr = mgr.clone();
    let mut harness = Harness::builder()
        .with_size([1180.0, 740.0])
        .build_eframe(move |cc| GuiApp::new(app_mgr, cc.egui_ctx.clone()));
    harness.run();
    screenshot(&mut harness, "01-setup");

    type_into(&mut harness, "Password", "adminpass1");
    type_into(&mut harness, "Confirm", "adminpass1");
    harness.get_by_label("Create admin").click();
    harness.run();
    if let super::Screen::Setup { error, password, confirm, .. } = &harness.state().screen {
        panic!("setup did not complete: error={error:?} password={password:?} confirm={confirm:?}");
    }
    harness.get_by_label("Users");
    harness.get_by_label("Audit log");

    let who = mgr.login("admin", "adminpass1").unwrap();
    let ssh_env = std::env::var("MGR_TEST_SSH").ok();
    let ssh = match &ssh_env {
        Some(spec) => {
            let parts: Vec<&str> = spec.splitn(4, ':').collect();
            SshConfig {
                host: parts[0].into(),
                port: parts[1].parse().unwrap(),
                username: parts[2].into(),
                auth: SshAuth::Key { path: parts[3].into() },
            }
        }
        None => SshConfig { host: "192.0.2.10".into(), ..Default::default() },
    };
    for (name, group) in [("web-01", "web"), ("db-01", "data")] {
        let input = ServerInput { name: name.into(), group: group.into(), ssh: Some(ssh.clone()), ..Default::default() };
        mgr.save_server(&who, None, input).unwrap();
    }
    harness.get_by_label("⟳ Refresh").click();
    run_until(&mut harness, Duration::from_secs(3), |_| false);
    harness.get_by_label("web-01");
    screenshot(&mut harness, "02-servers");

    harness.get_by_label("+ Add server").click();
    harness.run();
    harness.get_by_label("New server");
    screenshot(&mut harness, "03-add-server");
    harness.get_by_label("Cancel").click();
    harness.run();

    if ssh_env.is_some() {
        harness
            .get_all_by_role_and_label(egui::accesskit::Role::Button, "SSH")
            .next()
            .unwrap()
            .click();
        run_until(&mut harness, Duration::from_secs(5), |app| match &app.screen {
            super::Screen::Main(m) => m.sessions.first().is_some_and(|s| {
                s.term.with_screen(|sc| sc.contents().contains('$') || sc.contents().contains('❯'))
            }),
            _ => false,
        });
        // Click into the terminal, type a command and check the remote output.
        let center = egui::pos2(420.0, 270.0);
        harness.event(egui::Event::PointerMoved(center));
        for pressed in [true, false] {
            harness.event(egui::Event::PointerButton {
                pos: center,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
            harness.step();
        }
        harness.event(egui::Event::Text("echo gui-$((6*7))".into()));
        harness.event(egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let has_output = |app: &GuiApp| match &app.screen {
            super::Screen::Main(m) => m.sessions[0].term.with_screen(|sc| sc.contents().contains("gui-42")),
            _ => false,
        };
        run_until(&mut harness, Duration::from_secs(5), has_output);
        assert!(has_output(harness.state()), "typed command should run in the SSH session");
        screenshot(&mut harness, "04-ssh-session");
    }

    harness.get_by_label("Users").click();
    harness.run();
    harness.get_by_label("admin (you)");
    screenshot(&mut harness, "05-users");

    std::fs::remove_dir_all(dir).unwrap();
}
