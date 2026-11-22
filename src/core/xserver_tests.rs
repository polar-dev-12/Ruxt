use super::*;

#[test]
fn xauth_file_roundtrips_through_the_x11_cookie_reader() {
    let dir = std::env::temp_dir().join(format!("ruxt-xauth-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("xauth-7");
    let cookie = [0x5au8; 16];
    write_xauth(&path, 7, &cookie).unwrap();
    super::super::x11::set_managed_auth(7, path.clone());
    let t = super::super::x11::resolve("");
    assert_eq!(t.display_number, 7);
    assert_eq!(t.port, 6007);
    assert_eq!(t.real_cookie.as_deref(), Some(&cookie[..]));
    // a different display must not pick up the managed cookie
    let other = super::super::x11::resolve(":8");
    assert_ne!(other.real_cookie.as_deref(), Some(&cookie[..]));
    super::super::x11::clear_managed_auth();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn listening_probe_sees_a_local_listener() {
    // Bind a listener on a display port that is free, then probe.
    let free = (0u16..10).find(|n| std::net::TcpListener::bind(("127.0.0.1", 6000 + n)).is_ok());
    let Some(n) = free else { return };
    let l = std::net::TcpListener::bind(("127.0.0.1", 6000 + n)).unwrap();
    assert!(listening_displays().contains(&n));
    drop(l);
}

#[test]
fn only_xwin_is_accepted_as_an_override() {
    assert!(is_xwin(Path::new(r"C:\ruxt\xserver\bin\XWin.exe")));
    assert!(is_xwin(Path::new("/usr/bin/xwin.exe")));
    assert!(!is_xwin(Path::new(r"C:\Program Files\VcXsrv\vcxsrv.exe")));
    assert!(!is_xwin(Path::new(r"C:\Program Files (x86)\Xming\Xming.exe")));
    // An existing file that is not XWin.exe is ignored even when configured explicitly.
    let dir = std::env::temp_dir().join(format!("ruxt-xsrv-override-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let vcx = dir.join("vcxsrv.exe");
    std::fs::write(&vcx, b"").unwrap();
    std::env::set_var("RUXT_XSERVER_DIR", dir.join("none"));
    assert!(find_installed(vcx.to_str().unwrap()).is_none());
    std::env::remove_var("RUXT_XSERVER_DIR");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bundled_tree_is_searched_first() {
    let dir = std::env::temp_dir().join(format!("ruxt-xsrv-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(dir.join("bin").join("XWin.exe"), b"").unwrap();
    // SAFETY-free: tests in this module run single-threaded with respect to this variable.
    std::env::set_var("RUXT_XSERVER_DIR", &dir);
    let found = find_installed("").expect("bundled XWin should be found");
    std::env::remove_var("RUXT_XSERVER_DIR");
    assert!(found.bundled);
    assert_eq!(found.path, dir.join("bin").join("XWin.exe"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Starts the bundled Cygwin/X (`RUXT_XSERVER_DIR` must point at the tree) and runs Ruxt's own
/// untrusted-cookie generation and revocation against it:
/// `RUXT_XSERVER_DIR=target/release/xserver cargo test real_xserver -- --ignored`.
#[tokio::test]
#[ignore]
async fn real_xserver_mints_and_revokes_an_untrusted_cookie() {
    if std::env::var_os("RUXT_XSERVER_DIR").is_none() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("ruxt-xsrv-real-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut m = XServerManager::new(dir.clone(), String::new(), false);
    m.refresh(true);
    let display = m.start().expect("start bundled XWin");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !listening_displays().contains(&display) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let target = super::super::x11::resolve("");
    assert_eq!(target.display_number, display);
    let master = target.real_cookie.clone().expect("managed cookie");
    let auth = super::super::x11::generate_untrusted(&target, &master, 0)
        .await
        .expect("SECURITY extension present and generate works");
    assert_eq!(auth.cookie.len(), 16);
    assert_ne!(auth.cookie, master);
    super::super::x11::revoke_untrusted(&target, &master, auth.auth_id)
        .await
        .expect("revoke");
    m.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Starts the bundled server, waits, prints its windows (tasklist /V), stops it.
#[test]
#[ignore]
fn real_xserver_window_probe() {
    if std::env::var_os("RUXT_XSERVER_DIR").is_none() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("ruxt-xsrv-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut m = XServerManager::new(dir.clone(), String::new(), false);
    m.refresh(true);
    let display = m.start().expect("start");
    std::thread::sleep(Duration::from_secs(4));
    let out = std::process::Command::new("tasklist")
        .args(["/V", "/FI", "IMAGENAME eq XWin.exe"])
        .output()
        .unwrap();
    println!("display :{display}\n{}", String::from_utf8_lossy(&out.stdout));
    m.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Which creation flag actually hides XWin's console? Prints MainWindowHandle per variant.
/// Cookie-protected like the real launch (never `-ac`).
#[test]
#[ignore]
#[cfg(windows)]
fn real_xserver_console_flags() {
    use std::os::windows::process::CommandExt;
    let Some(dir) = std::env::var_os("RUXT_XSERVER_DIR") else {
        return;
    };
    let exe = std::path::PathBuf::from(dir).join("bin").join("XWin.exe");
    let tmp = std::env::temp_dir().join(format!("ruxt-xflags-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let auth = tmp.join("xauth-7");
    write_xauth(&auth, 7, &[0x42u8; 16]).unwrap();
    for (name, flags) in [
        ("CREATE_NO_WINDOW", 0x0800_0000u32),
        ("DETACHED_PROCESS", 0x0000_0008u32),
        ("BOTH", 0x0800_0008u32),
    ] {
        let mut c = Command::new(&exe);
        c.args([
            ":7",
            "-multiwindow",
            "-noclipboard",
            "-wgl",
            "-listen",
            "tcp",
            "-nolisten",
            "inet6",
            "-silent-dup-error",
            "-fp",
            "/usr/share/X11/fonts/misc,built-ins",
            "-auth",
        ]);
        c.arg(&auth);
        c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        c.creation_flags(flags);
        let mut child = c.spawn().unwrap();
        std::thread::sleep(Duration::from_secs(4));
        let out = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "Get-Process XWin -ErrorAction SilentlyContinue | ForEach-Object { \"$($_.Id) hwnd=$($_.MainWindowHandle) title=$($_.MainWindowTitle)\" }",
            ])
            .output()
            .unwrap();
        println!(
            "== {name} (pid {}):\n{}",
            child.id(),
            String::from_utf8_lossy(&out.stdout)
        );
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .output();
        let _ = child.wait();
        std::thread::sleep(Duration::from_secs(1));
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn a_starting_server_is_listed_but_never_connected_to() {
    // Display 9: nothing listens there in tests, so it can only show up through `STARTING`.
    *STARTING.lock() = Some((9, Instant::now() + Duration::from_secs(5)));
    assert!(is_starting(9));
    assert!(listening_displays().contains(&9));
    finish_starting(9);
    assert!(!is_starting(9));
    assert!(!listening_displays().contains(&9));
    // An expired start-up counts as ready.
    *STARTING.lock() = Some((9, Instant::now() - Duration::from_millis(1)));
    assert!(!is_starting(9));
}

#[test]
fn ready_marker_is_found_in_the_log() {
    let dir = std::env::temp_dir().join(format!("ruxt-xready-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("xwin-3.log");
    std::fs::write(&log, "winInitMultiWindowWM - DISPLAY=:3.0\n").unwrap();
    assert!(!wait_for_ready_marker(
        &log,
        Instant::now() + Duration::from_millis(120)
    ));
    std::fs::write(
        &log,
        format!("x\n[1.0] {READY_MARKER} and successfully opened the display.\n"),
    )
    .unwrap();
    assert!(wait_for_ready_marker(&log, Instant::now() + Duration::from_secs(1)));
    let _ = std::fs::remove_dir_all(&dir);
}
