//! `process.exit()` from extension JavaScript (PKGP-03): pi exits with that code, restores the
//! terminal and runs `session_shutdown{reason:"quit"}`, and `exit` listeners run once.
//!
//! Print mode exits directly. Interactive mode is driven through a real PTY so the terminal
//! restore (pi-tui's `atexit` hook) is observable in the output.

#![cfg(unix)]

use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use tempfile::TempDir;

fn write_extension(dir: &Path, body: &str) -> PathBuf {
    let ext = dir.join("ext.js");
    std::fs::write(&ext, body).unwrap();
    ext
}

fn agent_dir(dir: &Path) -> PathBuf {
    let agent = dir.join("agent");
    std::fs::create_dir_all(&agent).unwrap();
    agent
}

fn base_command(ext: &Path, agent: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pi"));
    command
        .args(["-ne", "-e", ext.to_str().unwrap()])
        .env("PI_CODING_AGENT_DIR", agent)
        .env("PI_OFFLINE", "1")
        .env("PI_SKIP_VERSION_CHECK", "1");
    command
}

/// `process.exit(3)` in print mode: exit code 3, and `exit` listeners run exactly once.
#[test]
fn print_mode_exits_with_the_code_and_runs_exit_listeners_once() {
    let dir = TempDir::new().unwrap();
    let marker = dir.path().join("listeners");
    let ext = write_extension(
        dir.path(),
        &format!(
            r#"import {{ appendFileSync }} from "node:fs";
            export default function (pi) {{
                pi.on("session_start", async () => {{
                    process.on("exit", () => appendFileSync({marker}, "exit\n"));
                    process.exit(3);
                }});
            }}"#,
            marker = serde_json::to_string(&marker.display().to_string()).unwrap()
        ),
    );
    let agent = agent_dir(dir.path());
    let output = base_command(&ext, &agent).args(["--mode", "json", "-p", "hi"]).output().unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "exit\n");
}

/// Interactive mode: `process.exit(0)` from `session_start` restores the terminal and emits
/// `session_shutdown{reason:"quit"}` before exiting 0.
#[test]
fn interactive_exit_restores_the_terminal_and_shuts_down_the_session() {
    let dir = TempDir::new().unwrap();
    let reason_file = dir.path().join("reason");
    let ext = write_extension(
        dir.path(),
        &format!(
            r#"import {{ writeFileSync }} from "node:fs";
            export default function (pi) {{
                const reasons = [];
                pi.on("session_shutdown", (e) => writeFileSync({reason:?}, [...reasons, e.reason].join(",")));
                pi.on("session_start", async () => {{ reasons.push("start"); process.exit(0); }});
            }}"#,
            reason = reason_file.display().to_string()
        ),
    );
    let agent = agent_dir(dir.path());
    let output = run_in_pty(base_command(&ext, &agent));
    assert_eq!(output.code, Some(0), "output: {:?}", tail(&output.stdout));
    assert_eq!(std::fs::read_to_string(&reason_file).unwrap(), "start,quit");
    // pi-tui's exit hook leaves the alt screen and shows the cursor again.
    assert!(output.stdout.contains("\x1b[?1049l"), "alt screen left: {:?}", tail(&output.stdout));
    assert!(output.stdout.contains("\x1b[?25h"), "cursor shown: {:?}", tail(&output.stdout));
    assert!(output.stdout.contains("\x1b[?2004l"), "bracketed paste off: {:?}", tail(&output.stdout));
}

fn tail(s: &str) -> &str {
    let start = s.len().saturating_sub(200);
    &s[start..]
}

struct PtyOutput {
    code: Option<i32>,
    /// Everything the process wrote to its terminal (stdout and stderr share the PTY).
    stdout: String,
}

/// Run `command` with a PTY as its stdin/stdout/stderr, like a terminal would.
fn run_in_pty(mut command: Command) -> PtyOutput {
    // SAFETY: openpty with a NULL termios asks libc for default terminal settings; the slave fd
    // is handed to the child, the master to this thread.
    let (master, slave) = unsafe {
        let mut master: libc::c_int = 0;
        let mut slave: libc::c_int = 0;
        let mut name = [0 as libc::c_char; 128];
        let mut ws: libc::winsize = std::mem::zeroed();
        ws.ws_row = 24;
        ws.ws_col = 80;
        assert_eq!(
            libc::openpty(&mut master, &mut slave, name.as_mut_ptr(), std::ptr::null(), &ws),
            0,
            "openpty: {}",
            std::io::Error::last_os_error()
        );
        (master, slave)
    };
    // The child gets the slave for all three streams; the master stays here for reading.
    // SAFETY: every descriptor comes from openpty/dup just above and is owned here.
    let slave = unsafe { std::fs::File::from_raw_fd(slave) };
    let (to_child, to_parent, from_child) = unsafe {
        (
            std::process::Stdio::from_raw_fd(libc::dup(slave.as_raw_fd())),
            std::process::Stdio::from_raw_fd(libc::dup(slave.as_raw_fd())),
            std::process::Stdio::from_raw_fd(libc::dup(slave.as_raw_fd())),
        )
    };
    let mut child = command
        .stdin(to_child)
        .stdout(to_parent)
        .stderr(from_child)
        .env("TERM", "xterm-256color")
        .env_remove("TMUX")
        .env_remove("STY")
        .env_remove("ZELLIJ")
        .spawn()
        .unwrap();
    // The parent's slave copy goes away with the child's end of the PTY, so reads on the master
    // fail with EIO once the child has exited.
    drop(slave);
    // SAFETY: the master fd is owned here and only read from this thread.
    let mut reader = unsafe { std::fs::File::from_raw_fd(master) };
    // Non-blocking: read until the child exits, then drain what it wrote. A blocking read would
    // wait forever when the child leaves no end-of-file behind (a forked grandchild shares the
    // PTY), so the exit status is the stop signal.
    // SAFETY: fcntl on the master fd owned above; F_GETFL/F_SETFL take an int.
    let flags = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }, 0);
    let mut bytes = Vec::new();
    let mut buf = [0u8; 8192];
    let started = Instant::now();
    let status = loop {
        match reader.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => bytes.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            // EAGAIN/EIO: nothing more right now.
            Err(_) => {}
        }
        if let Some(status) = child.try_wait().unwrap() {
            // Drain whatever the child wrote before it went away.
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buf[..n]);
            }
            break status;
        }
        assert!(started.elapsed() < Duration::from_secs(60), "pi did not exit");
        std::thread::sleep(Duration::from_millis(20));
    };
    PtyOutput { code: status.code(), stdout: String::from_utf8_lossy(&bytes).into_owned() }
}
