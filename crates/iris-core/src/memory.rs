//! Simple persistent note store — `~/.code-iris/notes.md`.
//!
//! Notes are appended as markdown list items with a timestamp.
//! `/memory` shows all notes; `/memory <text>` saves one.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

fn notes_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    Ok(home.join(".code-iris").join("notes.md"))
}

/// Append a note with an ISO-style timestamp.
pub fn add_note(text: &str) -> Result<()> {
    let path = notes_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let ts = {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // Format as YYYY-MM-DD HH:MM (UTC, good enough for notes)
        let s = secs;
        let (y, mo, d, h, mi) = unix_to_ymdhm(s);
        format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}")
    };
    let line = format!("- [{ts}] {}\n", text.trim());
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

/// Read all saved notes (raw markdown text).
pub fn load_notes() -> Result<String> {
    let path = notes_path()?;
    if !path.exists() {
        return Ok(String::new());
    }
    std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))
}

/// Return the path to the notes file (for display).
pub fn notes_file_path() -> Result<PathBuf> {
    notes_path()
}

// ── Minimal UTC timestamp helper (no chrono dep needed) ──────────────────────

fn unix_to_ymdhm(secs: u64) -> (u32, u32, u32, u32, u32) {
    let mi = (secs / 60) % 60;
    let h  = (secs / 3600) % 24;
    let days = secs / 86400;
    // Days since 1970-01-01
    let (y, mo, d) = days_to_ymd(days);
    (y, mo, d, h as u32, mi as u32)
}

fn days_to_ymd(mut days: u64) -> (u32, u32, u32) {
    let mut y = 1970u32;
    loop {
        let dy = if is_leap(y) { 366 } else { 365 };
        if days < dy { break; }
        days -= dy;
        y += 1;
    }
    let months = if is_leap(y) {
        [31u64, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut mo = 1u32;
    for &dm in &months {
        if days < dm { break; }
        days -= dm;
        mo += 1;
    }
    (y, mo, days as u32 + 1)
}

fn is_leap(y: u32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}
