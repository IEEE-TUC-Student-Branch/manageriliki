//! Live VNC tests, skipped unless configured:
//! - `MGR_TEST_VNC=host:port[:password]` a reachable VNC server
//! - `MGR_TEST_SSH=host:port:user:keyfile` (optional) to also test tunneling,
//!   where the VNC server must be reachable from the SSH host at the same host:port.

use std::sync::Arc;
use std::time::{Duration, Instant};

use manageriliki_core::*;

fn wait_for_frame(session: &VncSession) -> std::result::Result<(usize, usize), String> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        match session.state() {
            VncState::Closed(err) => return Err(err.unwrap_or_else(|| "closed".into())),
            VncState::Connected => {
                let ready = session.with_framebuffer(|fb| {
                    (fb.generation > 0).then_some((fb.width, fb.height))
                });
                if let Some(size) = ready {
                    return Ok(size);
                }
            }
            VncState::Connecting => {}
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("timed out".into())
}

#[test]
fn live_vnc() {
    let Ok(spec) = std::env::var("MGR_TEST_VNC") else { return };
    let parts: Vec<&str> = spec.splitn(3, ':').collect();
    let (host, port, password) = (parts[0], parts[1].parse::<u16>().unwrap(), parts.get(2).copied());

    let dir = std::env::temp_dir().join(format!("mgr-live-vnc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let m = Manager::open(&dir).unwrap();
    let admin = m.create_initial_admin("admin", "adminpass1").unwrap();
    let secret = |p: Option<&str>| p.map_or(SecretUpdate::Keep, |p| SecretUpdate::Set(p.into()));

    let direct = ServerInput {
        name: "vnc-direct".into(),
        vnc: Some(VncConfig { host: host.into(), port, via_ssh: false }),
        vnc_secret: secret(password),
        ..Default::default()
    };
    let direct = ServerInput { ssh: None, ..direct };
    let id = m.save_server(&admin, None, direct).unwrap();
    let session = m.open_vnc(&admin, id, Arc::new(|| {})).unwrap();
    let (w, h) = wait_for_frame(&session).expect("direct VNC should deliver a frame");
    assert!(w > 0 && h > 0);
    let lit = session.with_framebuffer(|fb| fb.pixels.as_chunks::<4>().0.iter().any(|p| p[0] | p[1] | p[2] != 0));
    assert!(lit, "framebuffer should contain non-black pixels");
    println!("direct: {w}x{h} desktop {:?}", session.desktop_name());

    // Input path, against a guest that echoes keystrokes to the screen
    // (`MGR_TEST_VNC_ECHO`): typing must change far more pixels than the
    // blinking text cursor does on its own.
    if std::env::var_os("MGR_TEST_VNC_ECHO").is_some() {
        let changed_after = |session: &VncSession, input: &dyn Fn()| {
            std::thread::sleep(Duration::from_millis(1500)); // let boot output settle
            let before = session.with_framebuffer(|fb| fb.pixels.clone());
            input();
            std::thread::sleep(Duration::from_millis(1500));
            session.with_framebuffer(|fb| {
                fb.pixels.as_chunks::<4>().0.iter().zip(before.as_chunks::<4>().0).filter(|(a, b)| a != b).count()
            })
        };
        let idle = changed_after(&session, &|| {});
        let typed = changed_after(&session, &|| {
            for _ in 0..40 {
                session.key(keysym::from_char('X'), true);
                session.key(keysym::from_char('X'), false);
            }
        });
        println!("pixels changed: idle {idle}, after typing {typed}");
        assert!(typed > 1000 && typed > idle * 10, "typed keys should be echoed by the guest");
    }
    drop(session);

    if password.is_some() {
        let mut wrong = ServerInput::from_server(&m.get_server(&admin, id).unwrap());
        wrong.vnc_secret = SecretUpdate::Set("wrongpass".into());
        m.save_server(&admin, Some(id), wrong).unwrap();
        let session = m.open_vnc(&admin, id, Arc::new(|| {})).unwrap();
        let err = wait_for_frame(&session).expect_err("wrong password must fail");
        assert!(err.contains("authentication failed"), "unexpected error: {err}");
        println!("wrong password: {err}");
    }

    if let Ok(ssh) = std::env::var("MGR_TEST_SSH") {
        let p: Vec<&str> = ssh.splitn(4, ':').collect();
        let tunneled = ServerInput {
            name: "vnc-tunnel".into(),
            ssh: Some(SshConfig {
                host: p[0].into(),
                port: p[1].parse().unwrap(),
                username: p[2].into(),
                auth: SshAuth::Key { path: p[3].into() },
            }),
            vnc: Some(VncConfig { host: host.into(), port, via_ssh: true }),
            vnc_secret: secret(password),
            ..Default::default()
        };
        let id = m.save_server(&admin, None, tunneled).unwrap();
        let session = m.open_vnc(&admin, id, Arc::new(|| {})).unwrap();
        let (w, h) = wait_for_frame(&session).expect("tunneled VNC should deliver a frame");
        println!("tunneled: {w}x{h}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}
