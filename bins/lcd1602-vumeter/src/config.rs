//! The settings, as the plugin keeps them in Volumio's configuration store:
//! a JSON object of `key: { type, value }` entries under
//! `/data/configuration/system_hardware/lcd1602_vumeter/config.json`. The
//! daemon reads that file itself, at start and when the plugin asks, so
//! there is one source of truth. Every key has a default; a file that is
//! missing or short of a key leaves the default.

use std::fs;
use std::path::Path;

use panel::backlight::RgbChip;
use panel::glyphs::Style;
use panel::hd44780::Controller;
use panel::transport::PinMap;
use serde_json::Value;

pub const DEFAULT_PATH: &str = "/data/configuration/system_hardware/lcd1602_vumeter/config.json";

/// Which measurement drives the bars.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Rms,
    Peak,
}

/// What a row shows while nothing plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdleRow {
    Text,
    Clock,
    Date,
    Blank,
}

/// How the standby screen is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdleLayout {
    /// Two rows, each as `idle_rows` says.
    Rows,
    /// The time in large digits across both rows; the date has no room.
    BigClock,
}

/// How the backlight colour is chosen while music plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RgbMode {
    /// One colour per player state.
    Fixed,
    /// From green through amber to red with the level.
    Level,
    /// The backlight stays dark.
    Off,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub bus: u8,
    /// `None` is auto-detection.
    pub controller: Option<Controller>,
    pub address: Option<u8>,
    pub pins: PinMap,
    pub contrast: u8,
    /// `None` is auto-detection; `Some(None)` is no RGB driver.
    pub rgb: Option<Option<(u8, RgbChip)>>,
    pub gain_db: f32,
    pub source: Source,
    pub range_db: f32,
    pub style: Style,
    pub peak_hold: bool,
    pub peak_hold_ms: u32,
    pub labels: bool,
    pub fps: u32,
    pub idle_layout: IdleLayout,
    pub idle_rows: [IdleRow; 2],
    pub idle_text: [String; 2],
    pub clock_format: String,
    pub date_format: String,
    pub backlight_idle_off_s: u32,
    pub rgb_mode: RgbMode,
    pub rgb_play: (u8, u8, u8),
    pub rgb_pause: (u8, u8, u8),
    pub rgb_idle: (u8, u8, u8),
    pub rgb_brightness: u8,
    pub show_lifecycle: bool,
    pub text_boot: [String; 2],
    pub text_ready: [String; 2],
    pub text_pause: [String; 2],
    pub text_shutdown: [String; 2],
    pub text_reboot: [String; 2],
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            bus: 1,
            controller: None,
            address: None,
            pins: PinMap::STANDARD,
            contrast: 40,
            rgb: None,
            gain_db: 0.0,
            source: Source::Rms,
            range_db: 40.0,
            style: Style::Blocks,
            peak_hold: true,
            peak_hold_ms: 1000,
            labels: true,
            fps: 20,
            idle_layout: IdleLayout::Rows,
            idle_rows: [IdleRow::Clock, IdleRow::Date],
            idle_text: ["Volumio".into(), String::new()],
            clock_format: "%H:%M".into(),
            date_format: "%a %d %b".into(),
            backlight_idle_off_s: 0,
            rgb_mode: RgbMode::Fixed,
            rgb_play: (255, 255, 255),
            rgb_pause: (255, 160, 0),
            rgb_idle: (40, 40, 60),
            rgb_brightness: 100,
            show_lifecycle: true,
            text_boot: lines("Volumio|booting..."),
            text_ready: lines("Volumio|ready"),
            text_pause: lines("Paused|waiting on music"),
            text_shutdown: lines("Volumio|shutting down"),
            text_reboot: lines("Volumio|restarting"),
        }
    }
}

/// Two rows from `a|b`; one row alone leaves the second empty.
pub fn lines(text: &str) -> [String; 2] {
    let mut it = text.splitn(2, '|');
    [
        it.next().unwrap_or("").trim_end().to_string(),
        it.next().unwrap_or("").trim_end().to_string(),
    ]
}

/// A colour from `RRGGBB` or `#RRGGBB`.
pub fn color(text: &str) -> Option<(u8, u8, u8)> {
    let hex = text.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some(((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// A 7-bit address from `0x3E`, `3E` or `62`.
pub fn address(text: &str) -> Option<u8> {
    let t = text.trim();
    let v = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u8::from_str_radix(h, 16).ok()?
    } else if t.chars().all(|c| c.is_ascii_digit()) {
        t.parse::<u8>().ok()?
    } else {
        u8::from_str_radix(t, 16).ok()?
    };
    (0x08..=0x77).contains(&v).then_some(v)
}

/// The store's entries, flattened to their values.
struct Store(Value);

impl Store {
    fn value(&self, key: &str) -> Option<&Value> {
        let entry = self.0.get(key)?;
        Some(entry.get("value").unwrap_or(entry))
    }

    fn string(&self, key: &str) -> Option<String> {
        match self.value(key)? {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    }

    fn number(&self, key: &str) -> Option<f64> {
        match self.value(key)? {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    fn boolean(&self, key: &str) -> Option<bool> {
        match self.value(key)? {
            Value::Bool(b) => Some(*b),
            Value::String(s) => match s.trim() {
                "true" | "1" | "on" => Some(true),
                "false" | "0" | "off" => Some(false),
                _ => None,
            },
            Value::Number(n) => Some(n.as_f64().unwrap_or(0.0) != 0.0),
            _ => None,
        }
    }
}

impl Settings {
    /// The settings in `path`, or the defaults when there is no file; a
    /// file that does not parse is an error, the caller keeps what it had.
    pub fn load(path: &Path) -> Result<Settings, String> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Settings::default()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let json: Value =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Settings::from_store(json))
    }

    pub fn from_store(json: Value) -> Settings {
        let s = Store(json);
        let d = Settings::default();
        let num = |key: &str, lo: f64, hi: f64, default: f64| -> f64 {
            s.number(key).map(|v| v.clamp(lo, hi)).unwrap_or(default)
        };
        let text = |key: &str, default: &str| -> String {
            s.string(key)
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| default.to_string())
        };
        let two = |key: &str, default: &[String; 2]| -> [String; 2] {
            s.string(key)
                .map(|v| lines(&v))
                .unwrap_or_else(|| default.clone())
        };
        let rgb = |key: &str, default: (u8, u8, u8)| -> (u8, u8, u8) {
            s.string(key).and_then(|v| color(&v)).unwrap_or(default)
        };
        let idle_row = |key: &str, default: IdleRow| -> IdleRow {
            match s.string(key).as_deref() {
                Some("text") => IdleRow::Text,
                Some("clock") => IdleRow::Clock,
                Some("date") => IdleRow::Date,
                Some("blank") => IdleRow::Blank,
                _ => default,
            }
        };
        let contrast = num("contrast", 0.0, 63.0, d.contrast as f64) as u8;
        let controller = match s.string("panel_type").as_deref() {
            Some("aip31068") => Some(Controller::Aip31068),
            Some("st7032") => Some(Controller::St7032 { contrast }),
            Some("us2066") => Some(Controller::Us2066),
            Some("hd44780") | Some("pcf8574") => Some(Controller::Hd44780),
            Some("ws0010") => Some(Controller::Ws0010),
            _ => None,
        };
        let rgb_choice = match s.string("rgb_address").as_deref() {
            None | Some("auto") | Some("") => None,
            Some("none") => Some(None),
            Some(other) => address(other)
                .and_then(|a| RgbChip::at(a).map(|c| (a, c)))
                .map(Some),
        };
        Settings {
            bus: num("i2c_bus", 0.0, 99.0, d.bus as f64) as u8,
            controller,
            address: s.string("panel_address").and_then(|v| address(&v)),
            pins: s
                .string("pcf_pinmap")
                .and_then(|v| PinMap::by_name(&v))
                .unwrap_or(d.pins),
            contrast,
            rgb: rgb_choice,
            gain_db: num("gain_db", -12.0, 12.0, d.gain_db as f64) as f32,
            source: match s.string("meter_source").as_deref() {
                Some("peak") => Source::Peak,
                Some("rms") => Source::Rms,
                _ => d.source,
            },
            range_db: num("range_db", 20.0, 60.0, d.range_db as f64) as f32,
            style: s
                .string("bar_style")
                .and_then(|v| Style::by_name(&v))
                .unwrap_or(d.style),
            peak_hold: s.boolean("peak_hold").unwrap_or(d.peak_hold),
            peak_hold_ms: num("peak_hold_ms", 100.0, 5000.0, d.peak_hold_ms as f64) as u32,
            labels: s.boolean("labels").unwrap_or(d.labels),
            fps: num("fps", 5.0, 30.0, d.fps as f64) as u32,
            idle_layout: match s.string("idle_layout").as_deref() {
                Some("big_clock") => IdleLayout::BigClock,
                Some("rows") => IdleLayout::Rows,
                _ => d.idle_layout,
            },
            idle_rows: [
                idle_row("idle_row1", d.idle_rows[0]),
                idle_row("idle_row2", d.idle_rows[1]),
            ],
            idle_text: two("idle_text", &d.idle_text),
            clock_format: text("clock_format", &d.clock_format),
            date_format: text("date_format", &d.date_format),
            backlight_idle_off_s: num("backlight_idle_off_s", 0.0, 86400.0, 0.0) as u32,
            rgb_mode: match s.string("rgb_mode").as_deref() {
                Some("level") => RgbMode::Level,
                Some("off") => RgbMode::Off,
                Some("fixed") => RgbMode::Fixed,
                _ => d.rgb_mode,
            },
            rgb_play: rgb("rgb_play", d.rgb_play),
            rgb_pause: rgb("rgb_pause", d.rgb_pause),
            rgb_idle: rgb("rgb_idle", d.rgb_idle),
            rgb_brightness: num("rgb_brightness", 0.0, 100.0, d.rgb_brightness as f64) as u8,
            show_lifecycle: s.boolean("show_lifecycle").unwrap_or(d.show_lifecycle),
            text_boot: two("text_boot", &d.text_boot),
            text_ready: two("text_ready", &d.text_ready),
            text_pause: two("text_pause", &d.text_pause),
            text_shutdown: two("text_shutdown", &d.text_shutdown),
            text_reboot: two("text_reboot", &d.text_reboot),
        }
    }

    /// The display to open, where the settings name it whole; `None`
    /// where something is left to detection.
    pub fn spec(&self) -> Option<panel::Spec> {
        Some(panel::Spec {
            bus: self.bus,
            controller: self.controller?,
            address: self.address?,
            pins: self.pins,
            rgb: self.rgb?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_store_s_values_are_read_with_their_types_forgiven() {
        let json = json!({
            "i2c_bus": { "type": "number", "value": "3" },
            "panel_type": { "type": "string", "value": "st7032" },
            "panel_address": { "type": "string", "value": "0x3E" },
            "contrast": { "type": "number", "value": 99 },
            "rgb_address": { "type": "string", "value": "0x62" },
            "gain_db": { "type": "number", "value": -30 },
            "meter_source": { "type": "string", "value": "peak" },
            "bar_style": { "type": "string", "value": "needle" },
            "peak_hold": { "type": "boolean", "value": "false" },
            "idle_row1": { "type": "string", "value": "clock" },
            "idle_row2": { "type": "string", "value": "date" },
            "idle_layout": { "type": "string", "value": "big_clock" },
            "clock_format": { "type": "string", "value": "%I:%M %p" },
            "rgb_play": { "type": "string", "value": "#FF0080" },
            "rgb_mode": { "type": "string", "value": "level" },
            "text_pause": { "type": "string", "value": "Pause|" },
            "fps": 12
        });
        let s = Settings::from_store(json);
        assert_eq!(s.bus, 3);
        assert_eq!(s.controller, Some(Controller::St7032 { contrast: 63 }));
        assert_eq!(s.address, Some(0x3E));
        assert_eq!(s.rgb, Some(Some((0x62, RgbChip::Pca9633))));
        assert_eq!(s.gain_db, -12.0, "clamped");
        assert_eq!(s.source, Source::Peak);
        assert_eq!(s.style, Style::Needle);
        assert!(!s.peak_hold);
        assert_eq!(s.idle_rows, [IdleRow::Clock, IdleRow::Date]);
        assert_eq!(s.idle_layout, IdleLayout::BigClock);
        assert_eq!(s.clock_format, "%I:%M %p");
        assert_eq!(s.rgb_play, (255, 0, 128));
        assert_eq!(s.rgb_mode, RgbMode::Level);
        assert_eq!(s.text_pause, ["Pause".to_string(), String::new()]);
        assert_eq!(s.fps, 12, "a bare value without the type wrapper");
        assert_eq!(
            s.text_boot,
            Settings::default().text_boot,
            "untouched keys keep defaults"
        );
        assert!(
            s.spec().is_some(),
            "controller, address and RGB named: a full spec"
        );
    }

    #[test]
    fn auto_and_none_are_told_apart_for_the_rgb_driver() {
        let auto = Settings::from_store(json!({ "rgb_address": { "value": "auto" } }));
        assert_eq!(auto.rgb, None);
        assert!(auto.spec().is_none());
        let none = Settings::from_store(json!({ "rgb_address": { "value": "none" } }));
        assert_eq!(none.rgb, Some(None));
        let unknown = Settings::from_store(json!({ "rgb_address": { "value": "0x11" } }));
        assert_eq!(
            unknown.rgb, None,
            "an address no driver is known at falls back to auto"
        );
    }

    #[test]
    fn helpers_parse_what_the_settings_page_sends() {
        assert_eq!(color("#ffFF00"), Some((255, 255, 0)));
        assert_eq!(color("00FF00"), Some((0, 255, 0)));
        assert_eq!(color("fff"), None);
        assert_eq!(address("0x27"), Some(0x27));
        assert_eq!(address("3F"), Some(0x3F));
        assert_eq!(address("39"), Some(39));
        assert_eq!(address("0x00"), None);
        assert_eq!(lines("a|b"), ["a".to_string(), "b".to_string()]);
        assert_eq!(lines("a"), ["a".to_string(), String::new()]);
    }

    #[test]
    fn a_missing_file_is_the_defaults_and_a_broken_one_an_error() {
        let dir = std::env::temp_dir().join(format!("lcd1602-config-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            Settings::load(&dir.join("none.json")).unwrap(),
            Settings::default()
        );
        let broken = dir.join("broken.json");
        fs::write(&broken, "{ not json").unwrap();
        assert!(Settings::load(&broken).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
