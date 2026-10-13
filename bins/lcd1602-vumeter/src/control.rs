//! The control socket: the plugin tells the daemon the player's state,
//! asks for a message, a reload or a farewell, and may ask how it is. One
//! JSON object per line, over a Unix socket; the daemon answers only a
//! `ping`.
//!
//! From the plugin:
//! - `{"kind":"state","status":"play|pause|stop"}`
//! - `{"kind":"ready"}` once the plugin has started
//! - `{"kind":"message","lines":["row 1","row 2"],"hold_ms":2000}`
//! - `{"kind":"farewell","reason":"shutdown|reboot"}` before the system goes down
//! - `{"kind":"reload"}` after the settings changed
//! - `{"kind":"ping"}`, answered with `{"kind":"pong", ...}`

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

/// The service's runtime directory, made by systemd for the daemon's user
/// and removed when the service stops.
pub const RUN_DIR: &str = "/run/lcd1602-vumeter";
/// Where the socket goes when the daemon runs outside systemd.
pub const FALLBACK_PATH: &str = "/tmp/lcd1602-vumeter.sock";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Play,
    Pause,
    Stop,
}

impl Status {
    pub fn name(&self) -> &'static str {
        match self {
            Status::Play => "play",
            Status::Pause => "pause",
            Status::Stop => "stop",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Farewell {
    Shutdown,
    Reboot,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    State(Status),
    Ready,
    Message { lines: [String; 2], hold: Duration },
    Farewell(Farewell),
    Reload,
}

/// What the daemon says of itself to a `ping`.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub panel: Option<String>,
    pub panel_error: Option<String>,
    pub ring: bool,
    pub phase: String,
    pub status: String,
}

/// One line from the plugin as a command, or `None` for a line that is
/// not one (a ping included).
pub fn parse(line: &str) -> Option<Command> {
    let v: Value = serde_json::from_str(line).ok()?;
    let kind = v.get("kind")?.as_str()?;
    let text = |key: &str| v.get(key).and_then(Value::as_str).unwrap_or("");
    match kind {
        "state" => Some(Command::State(match text("status") {
            "play" => Status::Play,
            "pause" => Status::Pause,
            _ => Status::Stop,
        })),
        "ready" => Some(Command::Ready),
        "message" => {
            let lines = match v.get("lines").and_then(Value::as_array) {
                Some(rows) => [
                    rows.first()
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    rows.get(1)
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                ],
                None => crate::config::lines(text("text")),
            };
            let hold = v
                .get("hold_ms")
                .and_then(Value::as_u64)
                .unwrap_or(2000)
                .clamp(100, 600_000);
            Some(Command::Message {
                lines,
                hold: Duration::from_millis(hold),
            })
        }
        "farewell" => Some(Command::Farewell(match text("reason") {
            "reboot" | "restart" => Farewell::Reboot,
            _ => Farewell::Shutdown,
        })),
        "reload" => Some(Command::Reload),
        _ => None,
    }
}

fn is_ping(line: &str) -> bool {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|v| v.get("kind").and_then(Value::as_str).map(|k| k == "ping"))
        .unwrap_or(false)
}

/// Listen on `path` on a thread of its own, handing every command to `tx`
/// and answering pings from `report`. A socket file left by an earlier
/// run is replaced.
pub fn serve(path: &Path, tx: Sender<Command>, report: Arc<Mutex<Report>>) -> io::Result<()> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    let path = path.to_path_buf();
    std::thread::Builder::new()
        .name("control".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let tx = tx.clone();
                let report = report.clone();
                let _ = std::thread::Builder::new()
                    .name("control-conn".into())
                    .spawn(move || connection(stream, tx, report));
            }
            let _ = std::fs::remove_file(&path);
        })?;
    Ok(())
}

fn connection(stream: UnixStream, tx: Sender<Command>, report: Arc<Mutex<Report>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if is_ping(line) {
            let r = report.lock().map(|r| r.clone()).unwrap_or_default();
            let answer = json!({
                "kind": "pong",
                "version": env!("CARGO_PKG_VERSION"),
                "panel": r.panel,
                "panelError": r.panel_error,
                "ring": r.ring,
                "phase": r.phase,
                "status": r.status,
            });
            if writeln!(writer, "{answer}").is_err() {
                break;
            }
            continue;
        }
        match parse(line) {
            Some(cmd) => {
                if tx.send(cmd).is_err() {
                    break;
                }
            }
            None => eprintln!("control: not a command: {line}"),
        }
    }
}

/// Send one line to the daemon at `path`; with `reply`, wait for one line back.
pub fn send(path: &Path, message: &Value, reply: bool) -> io::Result<Option<String>> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    writeln!(stream, "{message}")?;
    stream.flush()?;
    if !reply {
        return Ok(None);
    }
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    Ok(Some(line.trim().to_string()))
}

/// The socket's path: under the runtime directory while the service has
/// one, else under /tmp. Daemon and clients agree because the directory
/// exists exactly while the service runs.
pub fn default_path() -> PathBuf {
    let dir = Path::new(RUN_DIR);
    if dir.is_dir() {
        dir.join("control.sock")
    } else {
        PathBuf::from(FALLBACK_PATH)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn lines_parse_into_commands() {
        assert_eq!(
            parse(r#"{"kind":"state","status":"play"}"#),
            Some(Command::State(Status::Play))
        );
        assert_eq!(
            parse(r#"{"kind":"state","status":"anything"}"#),
            Some(Command::State(Status::Stop))
        );
        assert_eq!(parse(r#"{"kind":"ready"}"#), Some(Command::Ready));
        assert_eq!(
            parse(r#"{"kind":"message","lines":["a","b"],"hold_ms":500}"#),
            Some(Command::Message {
                lines: ["a".into(), "b".into()],
                hold: Duration::from_millis(500)
            })
        );
        assert_eq!(
            parse(r#"{"kind":"message","text":"one|two"}"#),
            Some(Command::Message {
                lines: ["one".into(), "two".into()],
                hold: Duration::from_millis(2000)
            })
        );
        assert_eq!(
            parse(r#"{"kind":"farewell","reason":"reboot"}"#),
            Some(Command::Farewell(Farewell::Reboot))
        );
        assert_eq!(parse(r#"{"kind":"reload"}"#), Some(Command::Reload));
        assert_eq!(parse(r#"{"kind":"ping"}"#), None);
        assert_eq!(parse("not json"), None);
        assert!(is_ping(r#"{"kind":"ping"}"#));
    }

    #[test]
    fn the_socket_carries_commands_and_answers_a_ping() {
        let path =
            std::env::temp_dir().join(format!("lcd1602-control-{}.sock", std::process::id()));
        let (tx, rx) = mpsc::channel();
        let report = Arc::new(Mutex::new(Report {
            panel: Some("sim".into()),
            phase: "player".into(),
            ..Report::default()
        }));
        serve(&path, tx, report).unwrap();
        send(&path, &json!({"kind":"state","status":"pause"}), false).unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Command::State(Status::Pause)
        );
        let answer = send(&path, &json!({"kind":"ping"}), true).unwrap().unwrap();
        let v: Value = serde_json::from_str(&answer).unwrap();
        assert_eq!(v["kind"], "pong");
        assert_eq!(v["panel"], "sim");
        assert_eq!(v["phase"], "player");
        let _ = std::fs::remove_file(&path);
    }
}
