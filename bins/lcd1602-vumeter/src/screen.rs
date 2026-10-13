//! What the glass shows in each state: the two bars, a message in two
//! rows, and the standby screen of text, clock and date.

use std::ffi::{c_char, CString};
use std::io;

use panel::glyphs;
use panel::{Bank, Panel, COLUMNS};

use crate::config::{IdleLayout, IdleRow, Settings};
use crate::meter::Meter;

/// The two bars, a label before each where asked.
pub fn bars(panel: &mut Panel, meter: &Meter, labels: bool, peak_hold: bool) {
    let cells = if labels { COLUMNS - 1 } else { COLUMNS };
    let total = cells as u16 * panel.glyphs().style().steps_per_cell();
    for (row, label) in b"LR".iter().enumerate() {
        let (level, peak) = meter.steps(row, total, peak_hold);
        let bar = panel.glyphs().bar(level, peak, cells);
        let mut line = Vec::with_capacity(COLUMNS);
        if labels {
            line.push(*label);
        }
        line.extend(bar);
        panel.set_row(row, &line);
    }
}

/// Two rows of text, each centred.
pub fn lines(panel: &mut Panel, text: &[String; 2]) {
    panel.set_centered(0, &text[0]);
    panel.set_centered(1, &text[1]);
}

/// The standby screen as the settings ask, at time `at`. The large clock
/// needs its own glyphs; the bars' come back with [`bars`].
pub fn idle(panel: &mut Panel, settings: &Settings, at: Clock) -> io::Result<()> {
    match settings.idle_layout {
        IdleLayout::Rows => {
            for row in 0..2 {
                let text = match settings.idle_rows[row] {
                    IdleRow::Text => settings.idle_text[row].clone(),
                    IdleRow::Clock => at.format(&settings.clock_format),
                    IdleRow::Date => at.format(&settings.date_format),
                    IdleRow::Blank => String::new(),
                };
                panel.set_centered(row, &text);
            }
        }
        IdleLayout::BigClock => {
            panel.set_bank(Bank::Digits)?;
            big_clock(panel, &settings.clock_format, at);
        }
    }
    Ok(())
}

/// The time in large digits, centred, with the half of the day beside it
/// where the clock format asks for a twelve-hour clock.
fn big_clock(panel: &mut Panel, clock_format: &str, at: Clock) {
    let twelve = clock_format.contains("%I") || clock_format.contains("%l");
    let time = at.format(if twelve { "%I:%M" } else { "%H:%M" });
    let half = if twelve {
        at.format("%p")
    } else {
        String::new()
    };
    let [mut top, mut bottom] = glyphs::big_text(&time);
    let width = top.len() + if half.is_empty() { 0 } else { half.len() + 1 };
    let pad = COLUMNS.saturating_sub(width) / 2;
    let mut t = vec![glyphs::SPACE; pad];
    let mut b = vec![glyphs::SPACE; pad];
    t.append(&mut top);
    b.append(&mut bottom);
    if !half.is_empty() {
        t.push(glyphs::SPACE);
        t.extend(half.bytes().take(2));
    }
    panel.set_row(0, &t);
    panel.set_row(1, &b);
}

/// The pause rows: the text, with the pause glyph in the first cell and
/// the first row centred in what is left.
pub fn paused(panel: &mut Panel, text: &[String; 2]) {
    lines(panel, text);
    let n = text[0].chars().count().min(COLUMNS - 2);
    let pad = ((COLUMNS - n) / 2).max(2);
    let mut row = vec![glyphs::SPACE; COLUMNS];
    row[0] = glyphs::PAUSE;
    for (i, c) in text[0].chars().take(n).enumerate() {
        if pad + i < COLUMNS {
            row[pad + i] = if c.is_ascii() && !c.is_ascii_control() {
                c as u8
            } else {
                b'?'
            };
        }
    }
    panel.set_row(0, &row);
}

/// A moment to format, the system's clock or a fixed one for pictures.
#[derive(Clone, Copy, Debug)]
pub enum Clock {
    Now,
    /// Seconds since the epoch, formatted in UTC so a picture is the same anywhere.
    Fixed(i64),
}

impl Clock {
    /// `format` as `strftime` takes it; what does not fit a row is cut.
    pub fn format(self, format: &str) -> String {
        let Ok(cformat) = CString::new(format) else {
            return String::new();
        };
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe {
            match self {
                Clock::Now => {
                    let mut t: libc::time_t = 0;
                    libc::time(&mut t);
                    libc::localtime_r(&t, &mut tm);
                }
                Clock::Fixed(epoch) => {
                    let t: libc::time_t = epoch as libc::time_t;
                    libc::gmtime_r(&t, &mut tm);
                }
            }
        }
        let mut buf = [0u8; 64];
        let n = unsafe {
            libc::strftime(
                buf.as_mut_ptr() as *mut c_char,
                buf.len(),
                cformat.as_ptr(),
                &tm,
            )
        };
        String::from_utf8_lossy(&buf[..n])
            .chars()
            .take(COLUMNS)
            .collect()
    }
}

/// Read the time zone once, so a change on the player is seen.
pub fn tzset() {
    extern "C" {
        fn tzset();
    }
    unsafe { tzset() };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Source;
    use panel::glyphs::Style;
    use std::time::Duration;
    use vutap::Frame;

    #[test]
    fn bars_carry_their_labels_and_fill_with_the_level() {
        let (mut panel, memory) = Panel::simulated(Style::Blocks);
        let mut meter = Meter::new(Source::Peak, 0.0, 40.0, Duration::from_secs(1));
        let frame = Frame {
            peak: [1.0, 0.0],
            rms: [1.0, 0.0],
            ..Frame::default()
        };
        for _ in 0..50 {
            meter.feed(&frame, Duration::from_millis(20));
        }
        bars(&mut panel, &meter, true, true);
        panel.flush().unwrap();
        let m = memory.borrow();
        assert_eq!(m.text(0, 16), "L@@@@@@@@@@@@@@@");
        assert_eq!(m.text(1, 16), "R               ");
        drop(m);
        bars(&mut panel, &meter, false, false);
        panel.flush().unwrap();
        assert_eq!(memory.borrow().text(0, 16), "@@@@@@@@@@@@@@@@");
    }

    #[test]
    fn messages_are_centred_and_a_pause_carries_its_mark() {
        let (mut panel, memory) = Panel::simulated(Style::Blocks);
        lines(&mut panel, &["Volumio".into(), "ready".into()]);
        panel.flush().unwrap();
        assert_eq!(memory.borrow().text(0, 16), "    Volumio     ");
        assert_eq!(memory.borrow().text(1, 16), "     ready      ");
        paused(&mut panel, &["Paused".into(), "waiting on music".into()]);
        panel.flush().unwrap();
        assert_eq!(memory.borrow().text(0, 16), "#    Paused     ");
        assert_eq!(memory.borrow().text(1, 16), "waiting on music");
    }

    #[test]
    fn the_standby_rows_follow_the_settings_and_a_fixed_clock_is_the_same_anywhere() {
        let (mut panel, memory) = Panel::simulated(Style::Blocks);
        let mut settings = Settings {
            idle_rows: [IdleRow::Clock, IdleRow::Date],
            clock_format: "%H:%M".into(),
            date_format: "%Y-%m-%d".into(),
            ..Settings::default()
        };
        // 2026-10-13 20:45:00 UTC
        let at = Clock::Fixed(1_791_924_300);
        idle(&mut panel, &settings, at).unwrap();
        panel.flush().unwrap();
        assert_eq!(memory.borrow().text(0, 16), "     20:45      ");
        assert_eq!(memory.borrow().text(1, 16), "   2026-10-13   ");
        settings.idle_rows = [IdleRow::Text, IdleRow::Blank];
        idle(&mut panel, &settings, at).unwrap();
        panel.flush().unwrap();
        assert_eq!(memory.borrow().text(0, 16), "    Volumio     ");
        assert_eq!(memory.borrow().text(1, 16), "                ");
        assert!(!Clock::Now.format("%H:%M").is_empty());
    }

    #[test]
    fn the_large_clock_takes_both_rows_and_its_own_glyphs() {
        let (mut panel, memory) = Panel::simulated(Style::Blocks);
        let settings = Settings {
            idle_layout: IdleLayout::BigClock,
            ..Settings::default()
        };
        let at = Clock::Fixed(1_791_924_300);
        idle(&mut panel, &settings, at).unwrap();
        panel.flush().unwrap();
        assert_eq!(panel.bank(), Bank::Digits);
        // 20:45 in thirteen cells, a cell of margin on the left; every
        // glyph cell reads as # here, the 4's open top as spaces.
        assert_eq!(memory.borrow().text(0, 16), " #############  ");
        assert_eq!(memory.borrow().text(1, 16), " #######  ####  ");
        assert_eq!(
            memory.borrow().cgram[glyphs::BIG_BAR as usize],
            [0b11110; 8]
        );
        let twelve = Settings {
            idle_layout: IdleLayout::BigClock,
            clock_format: "%I:%M %p".into(),
            ..Settings::default()
        };
        idle(&mut panel, &twelve, at).unwrap();
        panel.flush().unwrap();
        assert_eq!(memory.borrow().text(0, 16), "############# PM");
        // Playing again: the bars' glyphs come back.
        let meter = Meter::new(Source::Peak, 0.0, 40.0, Duration::from_secs(1));
        panel.set_bank(Bank::Bars(Style::Blocks)).unwrap();
        bars(&mut panel, &meter, true, true);
        panel.flush().unwrap();
        assert_eq!(panel.bank(), Bank::Bars(Style::Blocks));
        assert_eq!(memory.borrow().text(0, 16), "L               ");
    }
}
