#![forbid(unsafe_code)]

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, TextStyle};
use metrics::{counter, histogram};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

static SYNTAX_SET: Lazy<syntect::parsing::SyntaxSet> =
    Lazy::new(|| syntect::parsing::SyntaxSet::load_defaults_newlines());
static THEME_SET: Lazy<syntect::highlighting::ThemeSet> =
    Lazy::new(|| syntect::highlighting::ThemeSet::load_defaults());

// Very small memoization to avoid rebuilding on identical text/theme pairs
static LRU: Lazy<Mutex<HashMap<u64, Arc<egui::Galley>>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static LAYOUT_CAP: Lazy<usize> = Lazy::new(|| {
    std::env::var("ORKA_YAML_LAYOUT_CACHE_CAP")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(128)
});

fn to_color32(c: syntect::highlighting::Color) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

fn hash_key(s: &str, dark: bool, wrap: f32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    dark.hash(&mut h);
    // Quantize wrap to avoid too many keys for tiny width changes
    let q = ((wrap / 8.0).round() as i32).max(0);
    q.hash(&mut h);
    h.finish()
}

pub fn yaml_layouter() -> impl FnMut(&egui::Ui, &dyn egui::TextBuffer, f32) -> Arc<egui::Galley> {
    move |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
        let dark = ui.style().visuals.dark_mode;
        let s = text.as_str();
        let key = hash_key(s, dark, wrap_width);
        if let Some(job) = LRU.lock().ok().and_then(|m| m.get(&key).cloned()) {
            counter!("yaml_layout_cache_hit", 1u64);
            return job;
        }
        counter!("yaml_layout_cache_miss", 1u64);
        let syn = SYNTAX_SET
            .find_syntax_by_extension("yaml")
            .or_else(|| SYNTAX_SET.find_syntax_by_extension("yml"))
            .unwrap_or_else(|| SYNTAX_SET.find_syntax_plain_text());
        // Pick a reasonable built-in theme based on mode
        let theme_name = if dark {
            "Solarized (dark)"
        } else {
            "Solarized (light)"
        };
        let theme = THEME_SET
            .themes
            .get(theme_name)
            .or_else(|| THEME_SET.themes.get("base16-ocean.dark"))
            .or_else(|| THEME_SET.themes.get("InspiredGitHub"));
        let theme = theme.unwrap_or_else(|| THEME_SET.themes.values().next().unwrap());
        let t0 = std::time::Instant::now();
        let mut h = syntect::easy::HighlightLines::new(syn, theme);
        let mut job = LayoutJob::default();
        job.wrap.max_width = wrap_width;
        let mono = TextStyle::Monospace.resolve(ui.style());
        for line in s.split_inclusive(['\n']) {
            let regions = h
                .highlight_line(line.trim_end_matches('\n'), &SYNTAX_SET)
                .unwrap_or_default();
            for (style, piece) in regions.into_iter() {
                let mut fmt = TextFormat {
                    font_id: mono.clone(),
                    color: to_color32(style.foreground),
                    ..Default::default()
                };
                if style
                    .font_style
                    .contains(syntect::highlighting::FontStyle::BOLD)
                {
                    fmt.font_id.size *= 1.0;
                }
                // italics unsupported by default egui fonts — ignore or emulate lightly
                job.append(piece, 0.0, fmt);
            }
            if line.ends_with('\n') {
                job.append(
                    "\n",
                    0.0,
                    TextFormat {
                        font_id: mono.clone(),
                        color: ui.visuals().text_color(),
                        ..Default::default()
                    },
                );
            }
        }
        let galley = ui.fonts(|f| f.layout_job(job));
        let took_ms = t0.elapsed().as_millis() as f64;
        histogram!("yaml_layout_build_ms", took_ms);
        if let Ok(mut m) = LRU.lock() {
            if m.len() > *LAYOUT_CAP {
                m.clear();
            }
            m.insert(key, galley.clone());
        }
        galley
    }
}
