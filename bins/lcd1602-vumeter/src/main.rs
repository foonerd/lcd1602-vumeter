//! The display daemon. It opens the LCD the settings name (or finds one),
//! reads the tap's ring while the player plays, hears the player's state
//! from the plugin over the control socket, and draws: the bars, the pause
//! and standby rows, the lifecycle messages. It also serves the command
//! line: a message onto the glass, a probe of the bus, a test pattern, the
//! pictures for the documentation.

mod config;
mod control;
mod lifecycle;
mod meter;
mod screen;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use panel::glyphs::Style;
use panel::hd44780::Controller;
use panel::i2c::Device;
use panel::render::Theme;
use panel::transport::{Native, Pcf8574};
use panel::{Bank, Panel, Spec};
use serde_json::json;
use vutap::ring::{merge_hops, Reader};

use config::{IdleRow, RgbMode, Settings};
use control::{Command, Farewell, Report, Status};
use meter::Meter;
use screen::Clock;

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// With no word from the plugin, the boot rows give way to standby after this.
const BOOT_PATIENCE: Duration = Duration::from_secs(90);
/// How long the ready rows show.
const READY_HOLD: Duration = Duration::from_millis(2500);
/// Playing this long with no ring says the tap is not in the chain.
const NO_TAP_PATIENCE: Duration = Duration::from_secs(10);
/// A display that failed is tried again after this.
const PANEL_RETRY: Duration = Duration::from_secs(5);
/// How often a missing ring is looked for.
const RING_LOOK: Duration = Duration::from_millis(500);

fn log(msg: &str) {
    eprintln!("lcd1602-vumeter: {msg}");
}

enum Mode {
    Run,
    Say { text: String, hold_ms: u64 },
    Probe,
    Test,
    Ping,
    Render { dir: PathBuf, theme: Theme },
    Version,
    Help,
}

struct Args {
    config: PathBuf,
    socket: PathBuf,
    ring_dir: PathBuf,
    bus: Option<u8>,
    mode: Mode,
}

const USAGE: &str = "\
lcd1602-vumeter [options]              run the display
lcd1602-vumeter --say 'row 1|row 2'    put a message on the glass (through the daemon, else directly)
lcd1602-vumeter --probe                list the devices found on the bus
lcd1602-vumeter --test                 a test pattern for a few seconds
lcd1602-vumeter --ping                 ask the running daemon how it is
lcd1602-vumeter --render DIR           write the documentation pictures as SVG
lcd1602-vumeter --version

options:
  --config PATH   the settings (default /data/configuration/system_hardware/lcd1602_vumeter/config.json)
  --socket PATH   the control socket (default /run/lcd1602-vumeter/control.sock under systemd, else /tmp/lcd1602-vumeter.sock)
  --ring-dir DIR  where the tap's rings are (default /dev/shm)
  --bus N         the I2C bus, over the settings
  --hold MS       how long --say shows (default 3000)
  --theme NAME    for --render: blue, green, oled or rgb:RRGGBB (default blue)
";

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        config: PathBuf::from(config::DEFAULT_PATH),
        socket: control::default_path(),
        ring_dir: PathBuf::from(vutap::ring::DIR),
        bus: None,
        mode: Mode::Run,
    };
    let mut hold_ms = 3000u64;
    let mut theme = Theme::BLUE;
    let mut say: Option<String> = None;
    let mut render: Option<PathBuf> = None;
    let mut it = std::env::args().skip(1);
    let value = |it: &mut dyn Iterator<Item = String>, flag: &str| -> Result<String, String> {
        it.next().ok_or_else(|| format!("{flag} needs a value"))
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--config" => args.config = PathBuf::from(value(&mut it, "--config")?),
            "--socket" => args.socket = PathBuf::from(value(&mut it, "--socket")?),
            "--ring-dir" => args.ring_dir = PathBuf::from(value(&mut it, "--ring-dir")?),
            "--bus" => {
                args.bus = Some(
                    value(&mut it, "--bus")?
                        .parse()
                        .map_err(|_| "--bus needs a number".to_string())?,
                )
            }
            "--hold" => {
                hold_ms = value(&mut it, "--hold")?
                    .parse()
                    .map_err(|_| "--hold needs milliseconds".to_string())?
            }
            "--theme" => {
                let name = value(&mut it, "--theme")?;
                theme = Theme::by_name(&name).ok_or_else(|| format!("unknown theme {name}"))?;
            }
            "--say" => say = Some(value(&mut it, "--say")?),
            "--render" => render = Some(PathBuf::from(value(&mut it, "--render")?)),
            "--probe" => args.mode = Mode::Probe,
            "--test" => args.mode = Mode::Test,
            "--ping" => args.mode = Mode::Ping,
            "--version" | "-V" => args.mode = Mode::Version,
            "--help" | "-h" => args.mode = Mode::Help,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if let Some(text) = say {
        args.mode = Mode::Say { text, hold_ms };
    }
    if let Some(dir) = render {
        args.mode = Mode::Render { dir, theme };
    }
    Ok(args)
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("lcd1602-vumeter: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    let code = match &args.mode {
        Mode::Version => {
            println!("lcd1602-vumeter {VERSION}");
            0
        }
        Mode::Help => {
            print!("{USAGE}");
            0
        }
        Mode::Probe => probe(&args),
        Mode::Ping => ping(&args),
        Mode::Say { text, hold_ms } => say(&args, text, *hold_ms),
        Mode::Test => say(&args, "LCD1602 VU meter|test  0123456789", 4000),
        Mode::Render { dir, theme } => render(dir, *theme),
        Mode::Run => run(&args),
    };
    std::process::exit(code);
}

/// The settings, with the bus from the command line over them.
fn load_settings(args: &Args) -> Settings {
    let mut settings = match Settings::load(&args.config) {
        Ok(s) => s,
        Err(e) => {
            log(&format!("settings: {e}; the defaults are used"));
            Settings::default()
        }
    };
    if let Some(bus) = args.bus {
        settings.bus = bus;
    }
    settings
}

/// The display to open: what the settings name, the rest found on the bus.
fn resolve_spec(settings: &Settings) -> Result<Spec, String> {
    if let Some(spec) = settings.spec() {
        return Ok(spec);
    }
    let bus = settings.bus;
    if !panel::i2c::bus_exists(bus) {
        return Err(format!("/dev/i2c-{bus} does not exist"));
    }
    let (controller, address) = match (settings.controller, settings.address) {
        (Some(c), Some(a)) => (c, a),
        (Some(c), None) => {
            let candidates: &[u8] = match c {
                Controller::Hd44780 | Controller::Ws0010 => &Pcf8574::ADDRESSES,
                Controller::Us2066 => &Native::OLED_ADDRESSES,
                _ => &[Native::LCD_ADDRESS],
            };
            let a = candidates
                .iter()
                .copied()
                .find(|a| Device::present(bus, *a))
                .unwrap_or(candidates[0]);
            (c, a)
        }
        (None, Some(a)) => {
            let c = if a == Native::LCD_ADDRESS {
                Controller::Aip31068
            } else if Native::OLED_ADDRESSES.contains(&a) {
                Controller::Us2066
            } else {
                Controller::Hd44780
            };
            (c, a)
        }
        (None, None) => {
            let found = Spec::detect(bus)
                .ok_or_else(|| format!("no known display answers on i2c-{bus}"))?;
            (found.controller, found.address)
        }
    };
    let controller = match controller {
        Controller::St7032 { .. } => Controller::St7032 {
            contrast: settings.contrast,
        },
        c => c,
    };
    let rgb = match settings.rgb {
        Some(choice) => choice,
        None => Spec::detect_rgb(bus),
    };
    Ok(Spec {
        bus,
        controller,
        address,
        pins: settings.pins,
        rgb,
    })
}

fn open_panel(settings: &Settings) -> Result<Panel, String> {
    let spec = resolve_spec(settings)?;
    let mut panel = Panel::open(&spec, settings.style).map_err(|e| format!("{spec}: {e}"))?;
    if let Controller::St7032 { contrast } = spec.controller {
        let _ = panel.contrast(contrast);
    }
    Ok(panel)
}

fn probe(args: &Args) -> i32 {
    let settings = load_settings(args);
    let bus = settings.bus;
    if !panel::i2c::bus_exists(bus) {
        println!("i2c-{bus}: no such bus (/dev/i2c-{bus})");
        return 1;
    }
    println!("i2c-{bus}:");
    let known: Vec<(u8, &str)> = vec![
        (Native::LCD_ADDRESS, "AiP31068 / ST7032 LCD controller"),
        (Native::OLED_ADDRESSES[0], "US2066 OLED controller"),
        (Native::OLED_ADDRESSES[1], "US2066 OLED controller"),
        (Pcf8574::ADDRESSES[0], "PCF8574 backpack"),
        (Pcf8574::ADDRESSES[1], "PCF8574A backpack"),
    ];
    let mut any = false;
    for addr in 0x08..=0x77u8 {
        if !Device::present(bus, addr) {
            continue;
        }
        any = true;
        let what = known
            .iter()
            .find(|(a, _)| *a == addr)
            .map(|(_, w)| w.to_string())
            .or_else(|| {
                panel::backlight::RgbChip::at(addr).map(|c| format!("RGB backlight ({})", c.name()))
            })
            .unwrap_or_else(|| "unknown device".to_string());
        println!("  0x{addr:02X}  {what}");
    }
    if !any {
        println!("  nothing answers");
    }
    match resolve_spec(&settings) {
        Ok(spec) => println!("display: {spec}"),
        Err(e) => println!("display: {e}"),
    }
    0
}

fn ping(args: &Args) -> i32 {
    match control::send(&args.socket, &json!({ "kind": "ping" }), true) {
        Ok(Some(answer)) => {
            println!("{answer}");
            0
        }
        Ok(None) => 1,
        Err(e) => {
            println!("{{\"kind\":\"gone\",\"error\":\"{e}\"}}");
            1
        }
    }
}

/// A message through the daemon, or straight onto the glass when none runs.
fn say(args: &Args, text: &str, hold_ms: u64) -> i32 {
    let lines = config::lines(text);
    let message = json!({ "kind": "message", "lines": lines, "hold_ms": hold_ms });
    if control::send(&args.socket, &message, false).is_ok() {
        return 0;
    }
    let settings = load_settings(args);
    let mut panel = match open_panel(&settings) {
        Ok(p) => p,
        Err(e) => {
            log(&format!("no display: {e}"));
            return 1;
        }
    };
    screen::lines(&mut panel, &lines);
    if let Err(e) = panel.flush() {
        log(&format!("write failed: {e}"));
        return 1;
    }
    let (r, g, b) = scaled(settings.rgb_play, settings.rgb_brightness);
    let _ = panel.set_rgb(r, g, b);
    0
}

/// A colour at a brightness, 0 to 100 percent.
fn scaled(rgb: (u8, u8, u8), brightness: u8) -> (u8, u8, u8) {
    let s = |v: u8| (v as u32 * brightness.min(100) as u32 / 100) as u8;
    (s(rgb.0), s(rgb.1), s(rgb.2))
}

/// Green through amber to red with the level, for the backlight.
fn level_color(loudness: f32) -> (u8, u8, u8) {
    let x = loudness.clamp(0.0, 1.0);
    if x < 0.7 {
        ((255.0 * (x / 0.7)) as u8, 255, 0)
    } else {
        (255, (255.0 * (1.0 - (x - 0.7) / 0.3)) as u8, 0)
    }
}

/// Where the daemon is in its life.
#[derive(Clone, Debug, PartialEq)]
enum Phase {
    Boot,
    Ready { until: Instant },
    Player,
    Message { lines: [String; 2], until: Instant },
    Farewell,
}

impl Phase {
    fn name(&self) -> &'static str {
        match self {
            Phase::Boot => "boot",
            Phase::Ready { .. } => "ready",
            Phase::Player => "player",
            Phase::Message { .. } => "message",
            Phase::Farewell => "farewell",
        }
    }
}

struct Daemon {
    settings: Settings,
    settings_path: PathBuf,
    ring_dir: PathBuf,
    panel: Option<Panel>,
    panel_error: Option<String>,
    panel_retry_at: Instant,
    ring: Option<Reader>,
    ring_seq: u64,
    ring_looked_at: Instant,
    meter: Meter,
    phase: Phase,
    status: Status,
    status_since: Instant,
    started: Instant,
    report: Arc<Mutex<Report>>,
}

impl Daemon {
    fn new(settings: Settings, settings_path: PathBuf, ring_dir: PathBuf) -> Daemon {
        let now = Instant::now();
        let meter = Self::meter_for(&settings);
        Daemon {
            settings,
            settings_path,
            ring_dir,
            panel: None,
            panel_error: None,
            panel_retry_at: now,
            ring: None,
            ring_seq: 0,
            ring_looked_at: now - RING_LOOK,
            meter,
            phase: Phase::Boot,
            status: Status::Stop,
            status_since: now,
            started: now,
            report: Arc::new(Mutex::new(Report::default())),
        }
    }

    fn meter_for(settings: &Settings) -> Meter {
        Meter::new(
            settings.source,
            settings.gain_db,
            settings.range_db,
            Duration::from_millis(settings.peak_hold_ms as u64),
        )
    }

    fn frame(&self) -> Duration {
        Duration::from_millis(1000 / self.settings.fps.clamp(5, 30) as u64)
    }

    /// Open the display when there is none, no more often than the retry allows.
    fn ensure_panel(&mut self, now: Instant) {
        if self.panel.is_some() || now < self.panel_retry_at {
            return;
        }
        self.panel_retry_at = now + PANEL_RETRY;
        match open_panel(&self.settings) {
            Ok(p) => {
                log(&format!("display: {}", p.describe()));
                self.panel = Some(p);
                self.panel_error = None;
            }
            Err(e) => {
                if self.panel_error.as_deref() != Some(&e) {
                    log(&format!(
                        "no display: {e}; trying again every {}s",
                        PANEL_RETRY.as_secs()
                    ));
                }
                self.panel_error = Some(e);
            }
        }
    }

    /// A write that failed: the display is dropped and opened again later.
    fn lost_panel(&mut self, err: std::io::Error) {
        log(&format!("display lost: {err}"));
        self.panel = None;
        self.panel_error = Some(err.to_string());
        self.panel_retry_at = Instant::now() + PANEL_RETRY;
    }

    /// The hops since the last look, into the meter; nothing lets it decay.
    fn follow_ring(&mut self, now: Instant, dt: Duration) {
        let live = self.ring.as_ref().is_some_and(Reader::is_live);
        if !live && now.duration_since(self.ring_looked_at) >= RING_LOOK {
            self.ring_looked_at = now;
            self.ring = Reader::open_live(&self.ring_dir);
            if let Some(r) = &self.ring {
                // Start from the latest hop: what came before this look is not replayed.
                self.ring_seq = r.info().seq.saturating_sub(1);
                log(&format!("ring: {}", r.path().display()));
            }
        }
        let merged = self
            .ring
            .as_ref()
            .filter(|r| r.is_live())
            .and_then(|r| merge_hops(r.since(self.ring_seq).into_iter()));
        match merged {
            Some(frame) => {
                self.ring_seq = frame.seq;
                self.meter.feed(&frame, dt);
            }
            None => self.meter.decay(dt),
        }
    }

    fn ring_live(&self) -> bool {
        self.ring.as_ref().is_some_and(Reader::is_live)
    }

    fn apply(&mut self, cmd: Command, now: Instant) {
        match cmd {
            Command::State(status) => {
                if status != self.status {
                    self.status = status;
                    self.status_since = now;
                    if status != Status::Play {
                        self.meter.reset();
                    }
                }
                if self.phase == Phase::Boot {
                    self.phase = Phase::Player;
                }
            }
            Command::Ready => {
                if self.phase == Phase::Boot && self.settings.show_lifecycle {
                    self.phase = Phase::Ready {
                        until: now + READY_HOLD,
                    };
                } else if self.phase == Phase::Boot {
                    self.phase = Phase::Player;
                }
            }
            Command::Message { lines, hold } => {
                if self.phase != Phase::Farewell {
                    self.phase = Phase::Message {
                        lines,
                        until: now + hold,
                    };
                }
            }
            Command::Farewell(kind) => self.farewell(kind),
            Command::Reload => self.reload(),
        }
    }

    /// The last rows before the system goes down; they stay on the glass.
    fn farewell(&mut self, kind: Farewell) {
        self.phase = Phase::Farewell;
        let text = match kind {
            Farewell::Shutdown => self.settings.text_shutdown.clone(),
            Farewell::Reboot => self.settings.text_reboot.clone(),
        };
        if let Some(p) = self.panel.as_mut() {
            screen::lines(p, &text);
            if let Err(e) = p.flush() {
                self.lost_panel(e);
            }
        }
    }

    fn reload(&mut self) {
        let fresh = match Settings::load(&self.settings_path) {
            Ok(s) => s,
            Err(e) => {
                log(&format!("settings: {e}; kept as they were"));
                return;
            }
        };
        let reopen = fresh.bus != self.settings.bus
            || fresh.controller != self.settings.controller
            || fresh.address != self.settings.address
            || fresh.pins != self.settings.pins
            || fresh.rgb != self.settings.rgb;
        let meter = Self::meter_for(&fresh);
        self.settings = fresh;
        self.meter = meter;
        if reopen {
            if let Some(mut p) = self.panel.take() {
                p.clear();
                let _ = p.flush();
            }
            self.panel_retry_at = Instant::now();
            log("settings: the display is opened anew");
            return;
        }
        if let Some(p) = self.panel.as_mut() {
            if let Controller::St7032 { .. } = p.spec().controller {
                let _ = p.contrast(self.settings.contrast);
            }
            // The next frame loads the glyphs it needs; the glass is drawn anew.
            p.redraw();
        }
        log("settings: reloaded");
    }

    /// One frame: what the glass shows now, and the backlight.
    fn draw(&mut self, now: Instant) {
        if self.phase == Phase::Farewell {
            return;
        }
        if self.phase == Phase::Boot && now.duration_since(self.started) > BOOT_PATIENCE {
            self.phase = Phase::Player;
        }
        if let Phase::Ready { until } | Phase::Message { until, .. } = self.phase {
            if now >= until {
                self.phase = Phase::Player;
            }
        }
        let ring_live = self.ring.as_ref().is_some_and(Reader::is_live);
        let Some(panel) = self.panel.as_mut() else {
            return;
        };
        let s = &self.settings;
        let playing_blind = self.status == Status::Play
            && !ring_live
            && now.duration_since(self.status_since) > NO_TAP_PATIENCE;
        // Everything but the standby screen draws with the bars' glyphs.
        let drawn: std::io::Result<()> = match &self.phase {
            Phase::Boot if !s.show_lifecycle => screen::idle(panel, s, Clock::Now),
            Phase::Player if self.status == Status::Stop => screen::idle(panel, s, Clock::Now),
            other => panel.set_bank(Bank::Bars(s.style)).map(|_| match other {
                Phase::Boot => screen::lines(panel, &s.text_boot),
                Phase::Ready { .. } => screen::lines(panel, &s.text_ready),
                Phase::Message { lines, .. } => screen::lines(panel, lines),
                Phase::Farewell => {}
                Phase::Player => match self.status {
                    Status::Play if playing_blind => {
                        screen::lines(panel, &["Playing".into(), "no audio tap".into()])
                    }
                    Status::Play => screen::bars(panel, &self.meter, s.labels, s.peak_hold),
                    Status::Pause if s.show_lifecycle => screen::paused(panel, &s.text_pause),
                    Status::Pause => screen::bars(panel, &self.meter, s.labels, s.peak_hold),
                    Status::Stop => {}
                },
            }),
        };
        if let Err(e) = drawn {
            self.lost_panel(e);
            return;
        }
        let idle_dark = self.status == Status::Stop
            && self.phase == Phase::Player
            && s.backlight_idle_off_s > 0
            && now.duration_since(self.status_since)
                > Duration::from_secs(s.backlight_idle_off_s as u64);
        let color = if idle_dark || s.rgb_mode == RgbMode::Off {
            (0, 0, 0)
        } else {
            let base = match (&self.phase, self.status) {
                (Phase::Player, Status::Play) if s.rgb_mode == RgbMode::Level => {
                    level_color(self.meter.loudness())
                }
                (Phase::Player, Status::Play) => s.rgb_play,
                (Phase::Player, Status::Pause) => s.rgb_pause,
                (Phase::Player, Status::Stop) => s.rgb_idle,
                _ => s.rgb_play,
            };
            scaled(base, s.rgb_brightness)
        };
        // Standby rows all blank: the controller sleeps too while the light is off.
        let display_on = !idle_dark
            || s.idle_layout == config::IdleLayout::BigClock
            || s.idle_rows.iter().any(|r| *r != IdleRow::Blank);
        let result = panel
            .set_rgb(color.0, color.1, color.2)
            .and_then(|_| panel.display(display_on))
            .and_then(|_| panel.flush());
        if let Err(e) = result {
            self.lost_panel(e);
        }
    }

    fn publish_report(&self) {
        if let Ok(mut r) = self.report.lock() {
            r.panel = self.panel.as_ref().map(Panel::describe);
            r.panel_error = self.panel_error.clone();
            r.ring = self.ring_live();
            r.phase = self.phase.name().to_string();
            r.status = self.status.name().to_string();
        }
    }

    /// Asked to leave: the farewell the plugin gave, else what systemd's
    /// queue says, else a cleared glass.
    fn leave(&mut self) {
        if self.phase == Phase::Farewell {
            return;
        }
        match lifecycle::pending_shutdown() {
            Some(kind) if self.settings.show_lifecycle => {
                log(&format!("leaving: {kind:?}"));
                self.farewell(kind);
            }
            _ => {
                log("leaving: the glass is cleared");
                if let Some(p) = self.panel.as_mut() {
                    p.clear();
                    let _ = p.set_rgb(0, 0, 0);
                    let _ = p.flush();
                }
            }
        }
    }

    fn run(&mut self, rx: Receiver<Command>) {
        let mut last = Instant::now();
        loop {
            let now = Instant::now();
            let dt = now.duration_since(last);
            last = now;
            let signals = lifecycle::take();
            if signals & lifecycle::TERM != 0 {
                self.leave();
                return;
            }
            if signals & lifecycle::HUP != 0 {
                self.reload();
            }
            self.ensure_panel(now);
            if self.status == Status::Play && self.phase == Phase::Player {
                self.follow_ring(now, dt);
            } else {
                self.meter.decay(dt);
            }
            self.draw(now);
            self.publish_report();
            // Commands arriving during the frame's rest are taken as they come.
            let deadline = now + self.frame();
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                match rx.recv_timeout(left) {
                    Ok(cmd) => self.apply(cmd, Instant::now()),
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        }
    }
}

fn run(args: &Args) -> i32 {
    lifecycle::install();
    screen::tzset();
    let settings = load_settings(args);
    log(&format!(
        "{VERSION} starting: settings {}, socket {}, rings under {}",
        args.config.display(),
        args.socket.display(),
        args.ring_dir.display()
    ));
    let mut daemon = Daemon::new(settings, args.config.clone(), args.ring_dir.clone());
    let (tx, rx) = mpsc::channel();
    if let Err(e) = control::serve(&args.socket, tx, daemon.report.clone()) {
        log(&format!("control socket {}: {e}", args.socket.display()));
        return 1;
    }
    daemon.run(rx);
    let _ = std::fs::remove_file(&args.socket);
    0
}

type Scene = (String, Style, Box<dyn Fn(&mut Panel, &Meter)>);

/// The documentation's pictures: every screen the daemon draws, from the
/// simulated display, as SVG files under `dir`.
fn render(dir: &Path, theme: Theme) -> i32 {
    if let Err(e) = std::fs::create_dir_all(dir) {
        log(&format!("{}: {e}", dir.display()));
        return 1;
    }
    let settings = Settings::default();
    // 2026-10-13 20:45 UTC, for pictures that are the same on every machine.
    let at = Clock::Fixed(1_791_924_300);
    let text = |name: &str, lines: [String; 2]| -> Scene {
        (
            name.to_string(),
            Style::Blocks,
            Box::new(move |p, _| screen::lines(p, &lines)),
        )
    };
    let mut scenes: Vec<Scene> = vec![
        text("boot", settings.text_boot.clone()),
        text("ready", settings.text_ready.clone()),
        text("shutdown", settings.text_shutdown.clone()),
        text("reboot", settings.text_reboot.clone()),
        text("no-tap", ["Playing".into(), "no audio tap".into()]),
    ];
    let pause = settings.text_pause.clone();
    scenes.push((
        "pause".into(),
        Style::Blocks,
        Box::new(move |p, _| screen::paused(p, &pause)),
    ));
    for style in Style::ALL {
        scenes.push((
            format!("bars-{}", style.name()),
            style,
            Box::new(|p, m| screen::bars(p, m, true, true)),
        ));
    }
    scenes.push((
        "bars-no-labels".into(),
        Style::Blocks,
        Box::new(|p, m| screen::bars(p, m, false, true)),
    ));
    let clock_rows = Settings {
        idle_rows: [IdleRow::Text, IdleRow::Clock],
        ..settings.clone()
    };
    scenes.push((
        "idle-text-clock".into(),
        Style::Blocks,
        Box::new(move |p, _| screen::idle(p, &clock_rows, at).expect("simulated")),
    ));
    let date_rows = Settings {
        idle_rows: [IdleRow::Clock, IdleRow::Date],
        clock_format: "%H:%M:%S".into(),
        date_format: "%A %d %B".into(),
        ..settings.clone()
    };
    scenes.push((
        "idle-clock-date".into(),
        Style::Blocks,
        Box::new(move |p, _| screen::idle(p, &date_rows, at).expect("simulated")),
    ));
    let big = Settings {
        idle_layout: config::IdleLayout::BigClock,
        ..settings.clone()
    };
    scenes.push((
        "idle-big-clock".into(),
        Style::Blocks,
        Box::new(move |p, _| screen::idle(p, &big, at).expect("simulated")),
    ));
    let big12 = Settings {
        idle_layout: config::IdleLayout::BigClock,
        clock_format: "%I:%M %p".into(),
        ..settings.clone()
    };
    scenes.push((
        "idle-big-clock-12h".into(),
        Style::Blocks,
        Box::new(move |p, _| screen::idle(p, &big12, at).expect("simulated")),
    ));
    // A meter at a telling level: left near -6 dB with its peak held at
    // -2, right near -14 dB with its peak at -9.
    let mut meter = Meter::new(config::Source::Peak, 0.0, 40.0, Duration::from_secs(10));
    let loud = vutap::Frame {
        peak: [0.79, 0.35],
        rms: [0.79, 0.35],
        ..vutap::Frame::default()
    };
    let quiet = vutap::Frame {
        peak: [0.5, 0.2],
        rms: [0.5, 0.2],
        ..vutap::Frame::default()
    };
    for _ in 0..50 {
        meter.feed(&loud, Duration::from_millis(20));
    }
    for _ in 0..50 {
        meter.feed(&quiet, Duration::from_millis(20));
    }
    for (name, style, draw) in scenes {
        let (mut panel, memory) = Panel::simulated(style);
        draw(&mut panel, &meter);
        panel.flush().expect("the simulator never fails");
        let picture = panel::render::svg(&memory.borrow(), panel::COLUMNS, theme);
        let path = dir.join(format!("{name}.svg"));
        if let Err(e) = std::fs::write(&path, picture) {
            log(&format!("{}: {e}", path.display()));
            return 1;
        }
        println!("{}", path.display());
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backlight_follows_the_level_and_the_brightness() {
        assert_eq!(level_color(0.0), (0, 255, 0));
        assert_eq!(level_color(1.0), (255, 0, 0));
        let (r, g, b) = level_color(0.7);
        assert!(
            r >= 250 && g == 255 && b == 0,
            "amber at the knee: {r},{g},{b}"
        );
        assert_eq!(scaled((200, 100, 50), 50), (100, 50, 25));
        assert_eq!(scaled((200, 100, 50), 100), (200, 100, 50));
        assert_eq!(scaled((200, 100, 50), 0), (0, 0, 0));
    }

    #[test]
    fn the_daemon_moves_through_its_phases() {
        let dir = std::env::temp_dir().join(format!("lcd1602-daemon-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let settings = Settings {
            idle_rows: [IdleRow::Text, IdleRow::Clock],
            ..Settings::default()
        };
        let mut d = Daemon::new(settings, dir.join("config.json"), dir.clone());
        let (panel, memory) = Panel::simulated(Style::Blocks);
        d.panel = Some(panel);
        let t0 = Instant::now();
        d.draw(t0);
        assert_eq!(memory.borrow().text(1, 16), "   booting...   ");
        d.apply(Command::Ready, t0);
        d.draw(t0);
        assert_eq!(memory.borrow().text(1, 16), "     ready      ");
        d.draw(t0 + READY_HOLD + Duration::from_millis(1));
        assert_eq!(d.phase, Phase::Player);
        assert_eq!(
            memory.borrow().text(0, 16),
            "    Volumio     ",
            "standby: the text row"
        );
        d.apply(Command::State(Status::Pause), t0);
        d.draw(t0);
        assert_eq!(memory.borrow().text(0, 16), "#    Paused     ");
        d.apply(Command::State(Status::Play), t0);
        d.draw(t0);
        assert_eq!(
            memory.borrow().text(0, 16),
            "L               ",
            "bars, at rest without a ring"
        );
        d.draw(t0 + NO_TAP_PATIENCE + Duration::from_secs(1));
        assert_eq!(memory.borrow().text(1, 16), "  no audio tap  ");
        d.apply(
            Command::Message {
                lines: ["Hello".into(), "there".into()],
                hold: Duration::from_secs(1),
            },
            t0,
        );
        d.draw(t0);
        assert_eq!(memory.borrow().text(0, 16), "     Hello      ");
        d.apply(Command::Farewell(Farewell::Reboot), t0);
        assert_eq!(memory.borrow().text(1, 16), "   restarting   ");
        d.apply(Command::State(Status::Play), t0);
        d.draw(t0);
        assert_eq!(
            memory.borrow().text(1, 16),
            "   restarting   ",
            "a farewell stays"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_ring_drives_the_bars() {
        let dir = std::env::temp_dir().join(format!("lcd1602-ring-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut d = Daemon::new(Settings::default(), dir.join("config.json"), dir.clone());
        let (panel, memory) = Panel::simulated(Style::Blocks);
        d.panel = Some(panel);
        d.phase = Phase::Player;
        let t0 = Instant::now();
        d.apply(Command::State(Status::Play), t0);
        let mut writer = vutap::Writer::create(&dir, "test", 48_000, 2, 1024, 8).unwrap();
        let frame = vutap::Frame {
            peak: [1.0, 1.0],
            rms: [1.0, 1.0],
            ..vutap::Frame::default()
        };
        for i in 0..60 {
            writer.publish(&frame);
            let now = t0 + Duration::from_millis(50 * i);
            d.follow_ring(now, Duration::from_millis(50));
        }
        assert!(d.ring_live());
        d.draw(t0 + Duration::from_secs(3));
        assert_eq!(memory.borrow().text(0, 16), "L@@@@@@@@@@@@@@@");
        assert_eq!(memory.borrow().text(1, 16), "R@@@@@@@@@@@@@@@");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
