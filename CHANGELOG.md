# Changelog

All notable changes to the LCD1602 VU Meter are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.2] - 2026-10-26

- The settings page in German, French, Italian, Dutch, Spanish, Polish, Japanese and Chinese, following the player's language; any other language shows English. A language file is all or nothing, since Volumio takes the plugin's toasts and modals from the one file of the player's language without falling back to the English one; the check refuses a file that lacks a key, a placeholder or the `|` separator.

## [0.1.1] - 2026-10-26

- A pause becomes standby. Volumio's play button pauses rather than stops, so a paused player is the usual resting state, and the pause text stayed on the glass until the player was stopped outright. Two settings at the head of the Standby section: *Pause goes to standby* (on) and *Standby after a pause* (0 s, at once; up to an hour). The standby rows and colour then show, and the backlight goes off on the same schedule as after a stop. Off keeps the pause text until the player stops or plays.
- The decibel mapping goes through `ln` rather than `log10`, so a daemon built on a glibc 2.43 host no longer binds `log10f`'s new symbol version and the glibc gate passes everywhere.
- The plugin requires Volumio 4 (`engines.volumio`).

## [0.1.0] - 2026-10-13

- **The first release.** An audio tap in the player's ALSA chain (`vutap`) measures peak and RMS of every source without touching the sound, PCM, DoP and native DSD alike, into a ring under `/dev/shm`. A display daemon draws two VU bars with VU ballistics and a peak hold on a 16x2 character LCD over I2C, in four bar styles; shows the player's pause; shows a clock and date, or a large clock across both rows, while nothing plays; and says when the system boots, stands ready, restarts and shuts down. Displays: AiP31068 modules (Waveshare LCD1602 I2C and RGB, Grove LCD RGB, DFRobot RGB LCD), ST7032 and US2066 modules, and HD44780 or WS0010 panels on a PCF8574 backpack; RGB backlights on the PCA9633, SGM31323 and DFRobot drivers, fixed per state or following the level. The Volumio plugin: a settings page for the connection (hardware or software I2C bus, controller, addresses, backpack pins), the meter, the standby screen, the messages and the backlight; the I2C bus written to `/boot/userconfig.txt`; a systemd service for the daemon; binaries for armhf, arm64 and amd64 in one zip.
