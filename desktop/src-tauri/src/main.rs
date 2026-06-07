//! GTO Lab desktop shell: the existing Vite viz in a Tauri webview, with `gto-server`
//! as a managed sidecar - the server's lifecycle IS the app's lifecycle. This retires
//! the two-terminal run procedure and the orphan-server / "solve server unreachable"
//! failure modes the project has documented repeatedly.
//!
//! Lifecycle (all rust-side; nothing shell-shaped is exposed to the webview):
//!   1. Probe 127.0.0.1:7474. Already-healthy gto-server -> REUSE it (no spawn; title
//!      notes "external server") - operator-friendly when a dev server is running.
//!      Port bound but NOT healthy -> FAIL LOUDLY (dialog) and exit; never silently
//!      spawn a second server against a squatted port.
//!   2. Resolve the repo root: env GTO_LAB_ROOT, else the baked default below (this is
//!      a PERSONAL, undistributed build; the baked absolute path is the honest default
//!      and the env var is the override).
//!   3. MATRIX GUARD (fail loudly, never cold-start): the resolved root must contain
//!      phase1.toml and matrix/manifest.json. Missing -> dialog naming the resolved
//!      path and exit. A stale baked path must NEVER degrade into silently re-solving
//!      the ~3.4GB benchmark from an empty cache.
//!   4. Spawn the provisioned sidecar (gto-server.exe beside this exe; see
//!      desktop/provision-sidecar.ps1) with current_dir = repo root, so phase1.toml
//!      and the matrix resolve EXACTLY as the standalone server does - one cache,
//!      shared with the gates and batch tools.
//!   5. Poll /api/health until ready (~10s budget) before proceeding.
//!   6. RunEvent::ExitRequested / Exit -> kill the child. gto-server is a single
//!      process (verified: zero child spawns), so the direct kill IS the tree-kill;
//!      the verify_desktop gate proves no survivor remains rather than assuming it.
//!
//! The webview origin on Windows is http://tauri.localhost (useHttpsScheme defaults
//! false in v2), so the viz's fetch/SSE to http://127.0.0.1:7474 is same-scheme (no
//! mixed-content block) and the server's permissive CORS admits the origin.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{Manager, RunEvent};

/// Personal-build default; override with GTO_LAB_ROOT. Distribution is out of scope,
/// so baking this machine's repo path is the honest resolution (plan, packaging phase).
const BAKED_DEFAULT_ROOT: &str = r"C:\Users\james\Projects\gto-poker-bot";

const HEALTH_ADDR: &str = "127.0.0.1:7474";

/// The Vite dev server addr. A dev-mode binary (custom-protocol feature OFF) makes the
/// webview navigate here (devUrl); that is correct only under `tauri dev`, where Vite is
/// already up. See `dev_server_up` and the dev-mode guard in `main`.
const DEV_SERVER_ADDR: &str = "127.0.0.1:5173";

/// CREATE_NO_WINDOW: gto-server is a console-subsystem exe; without this flag a GUI parent
/// (this app, double-clicked) spawns it WITH a console window, which violates "no terminals".
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

struct ServerChild(Mutex<Option<Child>>);

#[derive(PartialEq)]
enum PortState {
    Free,
    HealthyServer,
    BoundNotHealthy,
}

/// Minimal raw HTTP/1.1 health probe (std-only; no HTTP client dependency). Connect
/// refused = the port is free; a 200 from /api/health = a live gto-server; anything
/// else listening = a squatter.
fn probe_port() -> PortState {
    let stream = TcpStream::connect_timeout(
        &HEALTH_ADDR.parse().expect("static addr parses"),
        Duration::from_millis(700),
    );
    let mut stream = match stream {
        Ok(s) => s,
        Err(_) => return PortState::Free,
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(1500)));
    let req = format!("GET /api/health HTTP/1.1\r\nHost: {HEALTH_ADDR}\r\nConnection: close\r\n\r\n");
    if stream.write_all(req.as_bytes()).is_err() {
        return PortState::BoundNotHealthy;
    }
    let mut buf = String::new();
    let _ = stream.take(4096).read_to_string(&mut buf);
    if buf.starts_with("HTTP/1.1 200") && buf.contains("\"ok\":true") {
        PortState::HealthyServer
    } else {
        PortState::BoundNotHealthy
    }
}

/// True if something is listening on the Vite dev addr (a live `tauri dev` session).
fn dev_server_up() -> bool {
    TcpStream::connect_timeout(
        &DEV_SERVER_ADDR.parse().expect("static addr parses"),
        Duration::from_millis(500),
    )
    .is_ok()
}

fn repo_root() -> PathBuf {
    std::env::var("GTO_LAB_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(BAKED_DEFAULT_ROOT))
}

/// Blocking native error dialog + process exit (used before/around the event loop).
/// MessageBoxW directly (windows-sys, already in Tauri's tree): works pre-event-loop,
/// no plugin or app handle needed. Also mirrors to stderr and to a note file beside the
/// exe so the failure is machine-checkable (verify_desktop asserts these paths).
fn die(title: &str, message: &str) -> ! {
    eprintln!("{title}: {message}");
    if let Ok(exe) = std::env::current_exe() {
        let note = exe.with_file_name("gto-lab-startup-error.txt");
        let _ = std::fs::write(&note, format!("{title}\r\n\r\n{message}\r\n"));
    }
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let text = wide(message);
    let caption = wide(title);
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            windows_sys::Win32::UI::WindowsAndMessaging::MB_OK
                | windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR
                // raise above the maximised app window if one already exists, so a
                // post-window failure dialog can never hide behind it
                | windows_sys::Win32::UI::WindowsAndMessaging::MB_TOPMOST
                | windows_sys::Win32::UI::WindowsAndMessaging::MB_SETFOREGROUND,
        );
    }
    std::process::exit(2);
}

fn main() {
    // ---- dev-mode guard: the webview-source failure class, made loud ----
    // The custom-protocol feature gates whether generate_context! embeds frontendDist
    // (../../viz/dist) or points the webview at devUrl (http://localhost:5173). With the
    // feature OFF this binary is dev-mode: the viz only loads when Vite is up. That is
    // correct under `tauri dev` (beforeDevCommand starts Vite, so dev_server_up() is true),
    // but a bare double-click of a dev-mode release exe has no Vite, so the webview would
    // render ERR_CONNECTION_REFUSED. Fail loudly naming the build mistake instead.
    if !cfg!(feature = "custom-protocol") && !dev_server_up() {
        die(
            "GTO Lab: this is a dev-mode build (the viz cannot load)",
            "This binary was built WITHOUT the custom-protocol feature, so its webview \
             points at the Vite dev server (http://localhost:5173) instead of the bundled \
             viz, and nothing is serving 5173.\r\n\r\nBuild the real artefact with:\r\n  \
             desktop\\build-desktop.ps1\r\n\r\n(plain `cargo build --release` in \
             desktop/src-tauri produces this dev-mode binary - use the script, which passes \
             --features custom-protocol).",
        );
    }

    // ---- pre-flight: port state, root resolution, matrix guard (all before the UI) ----
    let port = probe_port();
    if port == PortState::BoundNotHealthy {
        die(
            "GTO Lab: port 7474 is held by something that is not a healthy gto-server",
            "Close whatever holds 127.0.0.1:7474 (find it: netstat -ano | findstr 7474) \
             and relaunch. GTO Lab never spawns a second server against a squatted port.",
        );
    }

    let reuse_external = port == PortState::HealthyServer;
    let root = repo_root();

    if !reuse_external {
        // amendment 1: the matrix guard fails loudly, never cold-starts an empty cache
        let manifest = root.join("matrix").join("manifest.json");
        let config = root.join("phase1.toml");
        if !manifest.exists() || !config.exists() {
            die(
                "GTO Lab: matrix not found",
                &format!(
                    "Expected phase1.toml and matrix\\manifest.json under:\r\n  {}\r\n\r\n\
                     Set GTO_LAB_ROOT to the gto-poker-bot repo root (or fix the baked \
                     default in desktop/src-tauri/src/main.rs). Refusing to start the \
                     solver against an empty cache: that would silently re-solve the \
                     ~3.4GB benchmark instead of using the existing matrix.",
                    root.display()
                ),
            );
        }
    }

    tauri::Builder::default()
        .manage(ServerChild(Mutex::new(None)))
        .setup(move |app| {
            // belt-and-braces against tauri#11554 (maximized:true reported flaky)
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.maximize();
                if reuse_external {
                    let _ = window.set_title("GTO Lab - external server (not managed)");
                }
            }

            if !reuse_external {
                // the provisioned sidecar sits beside this exe (provision-sidecar.ps1)
                let exe_dir = std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(PathBuf::from))
                    .expect("current exe has a parent dir");
                let server_exe = exe_dir.join("gto-server.exe");
                if !server_exe.exists() {
                    die(
                        "GTO Lab: sidecar binary missing",
                        &format!(
                            "Expected the provisioned server at:\r\n  {}\r\n\r\nRun \
                             desktop/provision-sidecar.ps1 after building gto-server \
                             (cargo build --release -p gto-server).",
                            server_exe.display()
                        ),
                    );
                }

                let child = Command::new(&server_exe)
                    .current_dir(&root)
                    .creation_flags(CREATE_NO_WINDOW)
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap_or_else(|e| {
                        die("GTO Lab: failed to start the solver", &format!("{e}"))
                    });
                *app.state::<ServerChild>().0.lock().unwrap() = Some(child);

                // readiness: the server binds in well under a second; budget 10s
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    if probe_port() == PortState::HealthyServer {
                        break;
                    }
                    if Instant::now() > deadline {
                        // collect what the child said before dying, if it died
                        let mut detail = String::new();
                        if let Some(child) =
                            app.state::<ServerChild>().0.lock().unwrap().as_mut()
                        {
                            if let Some(stderr) = child.stderr.as_mut() {
                                let _ = stderr.take(8192).read_to_string(&mut detail);
                            }
                            let _ = child.kill();
                        }
                        die(
                            "GTO Lab: the solver did not become healthy within 10s",
                            &format!("server stderr (if any):\r\n{detail}"),
                        );
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building GTO Lab")
        .run(|app, event| {
            // the load-bearing teardown: the server dies with the app. gto-server is a
            // single process (no children), so kill() IS the tree-kill; verify_desktop
            // proves no survivor remains on 7474.
            if matches!(event, RunEvent::ExitRequested { .. } | RunEvent::Exit) {
                if let Some(mut child) = app.state::<ServerChild>().0.lock().unwrap().take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        });
}
