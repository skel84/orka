#![forbid(unsafe_code)]

use chrono::{DateTime, Utc};
use eframe::egui;
use once_cell::sync::Lazy;
use regex::Regex;

// Minimal ANSI SGR parsing for 0 (reset), 30–37 and 90–97 (foreground colors)
// Returns a LayoutJob with segments colored accordingly.
#[allow(dead_code)]
pub fn parse_line_to_job(
    line: &str,
    default_color: egui::Color32,
    colorize: bool,
) -> egui::text::LayoutJob {
    parse_line_to_job_hl(line, default_color, colorize, None, egui::Color32::YELLOW)
}

pub fn parse_line_to_job_hl(
    line: &str,
    default_color: egui::Color32,
    colorize: bool,
    highlight_re: Option<&Regex>,
    highlight_color: egui::Color32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    // Always use monospace for logs
    let fmt = egui::TextFormat {
        font_id: egui::FontId::monospace(12.0),
        color: default_color,
        ..Default::default()
    };

    let tailspin_on = colorize;

    let bytes = line.as_bytes();
    let mut i = 0;
    let mut cur_fmt = fmt.clone();
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // Find end of CSI 'm'
            let start = i + 2;
            if let Some(mut end) = bytes[start..].iter().position(|&b| b == b'm') {
                end += start;
                // Apply SGR
                let seq = &line[start..end];
                apply_sgr(seq, &mut cur_fmt, default_color);
                i = end + 1;
                continue;
            } else {
                // Malformed; stop parsing; append rest
                append_tailspin_segment_with_hl(
                    &mut job,
                    &line[i..],
                    &cur_fmt,
                    tailspin_on,
                    default_color,
                    highlight_re,
                    highlight_color,
                );
                break;
            }
        }
        // Accumulate until next ESC or end
        let seg_start = i;
        while i < bytes.len() {
            if bytes[i] == 0x1b {
                break;
            }
            i += 1;
        }
        let seg = &line[seg_start..i];
        if !seg.is_empty() {
            append_tailspin_segment_with_hl(
                &mut job,
                seg,
                &cur_fmt,
                tailspin_on,
                default_color,
                highlight_re,
                highlight_color,
            );
        }
    }
    job
}

fn apply_sgr(seq: &str, fmt: &mut egui::TextFormat, default_color: egui::Color32) {
    // Split numbers by ';'
    for part in seq.split(';') {
        if let Ok(code) = part.parse::<i32>() {
            match code {
                0 => {
                    // reset
                    fmt.color = default_color;
                    fmt.italics = false;
                    fmt.underline = egui::Stroke::NONE;
                    fmt.strikethrough = egui::Stroke::NONE;
                    // keep font_id
                }
                30 => fmt.color = egui::Color32::from_rgb(0x00, 0x00, 0x00), // black
                31 => fmt.color = egui::Color32::from_rgb(0xCC, 0x00, 0x00), // red
                32 => fmt.color = egui::Color32::from_rgb(0x00, 0x99, 0x00), // green
                33 => fmt.color = egui::Color32::from_rgb(0xCC, 0xAA, 0x00), // yellow
                34 => fmt.color = egui::Color32::from_rgb(0x00, 0x00, 0xCC), // blue
                35 => fmt.color = egui::Color32::from_rgb(0xAA, 0x00, 0xAA), // magenta
                36 => fmt.color = egui::Color32::from_rgb(0x00, 0xAA, 0xAA), // cyan
                37 => fmt.color = egui::Color32::from_rgb(0xCC, 0xCC, 0xCC), // white (light gray)
                90 => fmt.color = egui::Color32::from_rgb(0x55, 0x55, 0x55), // bright black
                91 => fmt.color = egui::Color32::from_rgb(0xFF, 0x55, 0x55), // bright red
                92 => fmt.color = egui::Color32::from_rgb(0x55, 0xFF, 0x55), // bright green
                93 => fmt.color = egui::Color32::from_rgb(0xFF, 0xFF, 0x55), // bright yellow
                94 => fmt.color = egui::Color32::from_rgb(0x55, 0x55, 0xFF), // bright blue
                95 => fmt.color = egui::Color32::from_rgb(0xFF, 0x55, 0xFF), // bright magenta
                96 => fmt.color = egui::Color32::from_rgb(0x55, 0xFF, 0xFF), // bright cyan
                97 => fmt.color = egui::Color32::from_rgb(0xFF, 0xFF, 0xFF), // bright white
                39 => fmt.color = default_color,
                _ => { /* ignore others for now */ }
            }
        }
    }
}

pub fn parse_timestamp_utc(line: &str) -> Option<DateTime<Utc>> {
    // Try to parse an RFC3339 timestamp prefix up to first whitespace
    let first = line.split_whitespace().next()?;
    if let Ok(dt) = DateTime::parse_from_rfc3339(first) {
        return Some(dt.with_timezone(&Utc));
    }
    None
}

fn append_tailspin_segment_with_hl(
    job: &mut egui::text::LayoutJob,
    seg: &str,
    base_fmt: &egui::TextFormat,
    enable: bool,
    default_color: egui::Color32,
    highlight_re: Option<&Regex>,
    highlight_color: egui::Color32,
) {
    if !enable {
        if let Some(re) = highlight_re {
            append_with_highlight(job, seg, base_fmt.clone(), re, highlight_color);
        } else {
            job.append(seg, 0.0, base_fmt.clone());
        }
        return;
    }
    static TS_RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r"(?x)
            (?P<level>(?i)\b(error|warn|warning|info|debug|trace)\b)
            |
            (?P<ts>\b\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})\b)
            |
            (?P<num>\b\d+(?:\.\d+)?\b)
            ",
        )
        .unwrap()
    });
    let mut idx = 0usize;
    // Collect tailspin-colored segments first
    let mut pieces: Vec<(std::borrow::Cow<'_, str>, egui::TextFormat)> = Vec::new();
    while let Some(caps) = TS_RE.captures_at(seg, idx) {
        let m = caps.get(0).unwrap();
        if m.start() > idx {
            pieces.push((
                std::borrow::Cow::from(&seg[idx..m.start()]),
                base_fmt.clone(),
            ));
        }
        let mut fmt = base_fmt.clone();
        // Only override color if the base format is the default (avoid fighting ANSI colors)
        let can_override = base_fmt.color == default_color;
        if let Some(g) = caps.name("level") {
            if g.start() == m.start() && g.end() == m.end() && can_override {
                let word = seg[m.start()..m.end()].to_ascii_lowercase();
                fmt.color = match word.as_str() {
                    "error" => egui::Color32::from_rgb(0xE0, 0x40, 0x40),
                    "warn" | "warning" => egui::Color32::from_rgb(0xE0, 0xB0, 0x40),
                    "info" => egui::Color32::from_rgb(0x40, 0xA0, 0x40),
                    "debug" => egui::Color32::from_rgb(0x50, 0x80, 0xD0),
                    "trace" => egui::Color32::from_gray(180),
                    _ => base_fmt.color,
                };
            }
        } else if let Some(g) = caps.name("ts") {
            if g.start() == m.start() && g.end() == m.end() && can_override {
                fmt.color = egui::Color32::from_gray(180);
            }
        } else if let Some(g) = caps.name("num") {
            if g.start() == m.start() && g.end() == m.end() && can_override {
                fmt.color = egui::Color32::from_rgb(0x40, 0xC0, 0xC0);
            }
        }
        pieces.push((std::borrow::Cow::from(m.as_str()), fmt));
        idx = m.end();
        if idx >= seg.len() {
            break;
        }
    }
    if idx < seg.len() {
        pieces.push((std::borrow::Cow::from(&seg[idx..]), base_fmt.clone()));
    }
    // Apply optional regex highlight across pieces
    if let Some(re) = highlight_re {
        for (text, fmt) in pieces.into_iter() {
            append_with_highlight(job, &text, fmt, re, highlight_color);
        }
    } else {
        for (text, fmt) in pieces.into_iter() {
            job.append(&text, 0.0, fmt);
        }
    }
}

fn append_with_highlight(
    job: &mut egui::text::LayoutJob,
    text: &str,
    base_fmt: egui::TextFormat,
    re: &Regex,
    hl_color: egui::Color32,
) {
    let mut last = 0usize;
    for m in re.find_iter(text) {
        let start = m.start();
        let end = m.end();
        if start == end {
            continue;
        } // skip zero-width
        if start > last {
            job.append(&text[last..start], 0.0, base_fmt.clone());
        }
        let mut fmt = base_fmt.clone();
        fmt.underline = egui::Stroke {
            width: 1.5,
            color: hl_color,
        };
        job.append(&text[start..end], 0.0, fmt);
        last = end;
    }
    if last < text.len() {
        job.append(&text[last..], 0.0, base_fmt);
    }
}
