#![forbid(unsafe_code)]

use eframe::egui;

// Minimal ANSI terminal powered by vte parser. Keeps a fixed-size grid; on overflow scrolls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AnsiColor {
    Default,
    Index(u8),
}

#[derive(Clone, Debug)]
struct Cell {
    ch: char,
    fg: AnsiColor,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: AnsiColor::Default,
        }
    }
}

struct AnsiTerm {
    cols: u16,
    rows: u16,
    cur_x: u16,
    cur_y: u16,
    grid: Vec<Cell>,
    fg: AnsiColor,
}

impl AnsiTerm {
    fn new(cols: u16, rows: u16) -> Self {
        let cap = (cols as usize) * (rows as usize);
        Self {
            cols,
            rows,
            cur_x: 0,
            cur_y: 0,
            grid: vec![Cell::default(); cap],
            fg: AnsiColor::Default,
        }
    }
    #[allow(dead_code)]
    fn resize(&mut self, cols: u16, rows: u16) {
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.grid.clear();
        self.grid
            .resize((cols as usize) * (rows as usize), Cell::default());
        self.cur_x = 0;
        self.cur_y = 0;
    }
    fn idx(&self, x: u16, y: u16) -> usize {
        (y as usize) * (self.cols as usize) + (x as usize)
    }
    fn put_char(&mut self, c: char) {
        if c == '\n' {
            self.newline();
            return;
        }
        if c == '\r' {
            self.cur_x = 0;
            return;
        }
        if c == '\t' {
            let tab = 4;
            let nx = ((self.cur_x as usize / tab) + 1) * tab;
            self.cur_x = (nx.min(self.cols as usize - 1)) as u16;
            return;
        }
        if self.cur_x >= self.cols {
            self.newline();
        }
        if self.cur_y >= self.rows {
            self.scroll_up(1);
            self.cur_y = self.rows - 1;
        }
        let idx = self.idx(self.cur_x, self.cur_y);
        if idx < self.grid.len() {
            self.grid[idx] = Cell { ch: c, fg: self.fg };
        }
        if self.cur_x + 1 >= self.cols {
            self.newline();
        } else {
            self.cur_x += 1;
        }
    }
    fn newline(&mut self) {
        self.cur_x = 0;
        if self.cur_y + 1 >= self.rows {
            self.scroll_up(1);
        } else {
            self.cur_y += 1;
        }
    }
    fn backspace(&mut self) {
        if self.cur_x > 0 {
            self.cur_x -= 1;
            let idx = self.idx(self.cur_x, self.cur_y);
            self.grid[idx] = Cell::default();
        }
    }
    fn clear_screen(&mut self) {
        for cell in &mut self.grid {
            *cell = Cell::default();
        }
        self.cur_x = 0;
        self.cur_y = 0;
    }
    fn clear_eol(&mut self) {
        let y = self.cur_y;
        for x in self.cur_x..self.cols {
            let idx = self.idx(x, y);
            self.grid[idx] = Cell::default();
        }
    }
    fn move_to(&mut self, col1: u16, row1: u16) {
        self.cur_x = col1.saturating_sub(1).min(self.cols.saturating_sub(1));
        self.cur_y = row1.saturating_sub(1).min(self.rows.saturating_sub(1));
    }
    fn move_rel(&mut self, dx: i16, dy: i16) {
        let nx = (self.cur_x as i16 + dx).clamp(0, self.cols as i16 - 1) as u16;
        let ny = (self.cur_y as i16 + dy).clamp(0, self.rows as i16 - 1) as u16;
        self.cur_x = nx;
        self.cur_y = ny;
    }
    fn scroll_up(&mut self, n: u16) {
        let n = n.min(self.rows);
        let cols = self.cols as usize;
        // let rows = self.rows as usize; // unused
        let count = n as usize * cols;
        if count >= self.grid.len() {
            for c in &mut self.grid {
                *c = Cell::default();
            }
            return;
        }
        self.grid.drain(0..count);
        self.grid
            .extend(std::iter::repeat_n(Cell::default(), count));
    }
    fn set_sgr(&mut self, params: &[i64]) {
        if params.is_empty() {
            self.fg = AnsiColor::Default;
            return;
        }
        let mut it = params.iter();
        while let Some(p) = it.next() {
            match *p {
                0 => {
                    self.fg = AnsiColor::Default;
                }
                30..=37 => {
                    self.fg = AnsiColor::Index(((*p as i32) - 30) as u8);
                }
                90..=97 => {
                    self.fg = AnsiColor::Index(((*p as i32) - 90 + 8) as u8);
                }
                39 => {
                    self.fg = AnsiColor::Default;
                }
                38 => {
                    // 256-color not implemented; skip params
                    let _ = it.next();
                    let _ = it.next();
                }
                _ => {}
            }
        }
    }
}

impl vte::Perform for AnsiTerm {
    fn print(&mut self, c: char) {
        self.put_char(c);
    }
    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => self.newline(),
            b'\r' => {
                self.cur_x = 0;
            }
            0x08 => self.backspace(),
            _ => {}
        }
    }
    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        _intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        let nums: Vec<i64> = params
            .iter()
            .map(|p| if p.is_empty() { 0 } else { p[0] as i64 })
            .collect();
        match action {
            'H' | 'f' => {
                let row = if !nums.is_empty() && nums[0] > 0 {
                    nums[0] as u16
                } else {
                    1
                };
                let col = if nums.len() >= 2 && nums[1] > 0 {
                    nums[1] as u16
                } else {
                    1
                };
                self.move_to(col, row);
            }
            'A' => {
                let n = if nums.is_empty() || nums[0] == 0 {
                    1
                } else {
                    nums[0] as i16
                };
                self.move_rel(0, -n);
            }
            'B' => {
                let n = if nums.is_empty() || nums[0] == 0 {
                    1
                } else {
                    nums[0] as i16
                };
                self.move_rel(0, n);
            }
            'C' => {
                let n = if nums.is_empty() || nums[0] == 0 {
                    1
                } else {
                    nums[0] as i16
                };
                self.move_rel(n, 0);
            }
            'D' => {
                let n = if nums.is_empty() || nums[0] == 0 {
                    1
                } else {
                    nums[0] as i16
                };
                self.move_rel(-n, 0);
            }
            'G' => {
                let col = if nums.is_empty() || nums[0] == 0 {
                    1
                } else {
                    nums[0] as u16
                };
                self.move_to(col, self.cur_y + 1);
            }
            'J' => {
                // Erase in display
                let mode = if nums.is_empty() { 0 } else { nums[0] };
                if mode == 2 {
                    self.clear_screen();
                } else if mode == 0 {
                    self.clear_eol();
                }
            }
            'K' => {
                self.clear_eol();
            }
            'm' => {
                self.set_sgr(&nums);
            }
            _ => {}
        }
    }
}

/// Wrapper for an optional terminal view. When the `term` feature is enabled
/// this delegates to egui_term; otherwise it provides a minimal no-op view that
/// compiles without the dependency.
pub struct UiTerminal {
    parser: vte::Parser,
    term: AnsiTerm,
    // last computed grid size (cols, rows) to help resize decisions
    pub last_cols: Option<u16>,
    pub last_rows: Option<u16>,
}

impl UiTerminal {
    pub fn new() -> Self {
        Self {
            parser: vte::Parser::new(),
            term: AnsiTerm::new(80, 24),
            last_cols: None,
            last_rows: None,
        }
    }

    /// Feed raw bytes into the terminal (ANSI sequences supported when feature is on).
    pub fn feed_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.parser.advance(&mut self.term, b);
        }
    }

    /// Render the terminal. Returns computed (cols, rows) of the grid if known,
    /// which can be used to send PTY resize.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> (u16, u16, bool) {
        // Resolve font metrics to estimate character cell size
        let mono = egui::TextStyle::Monospace;
        let char_w = ui
            .fonts(|f| f.glyph_width(&mono.resolve(ui.style()), 'W'))
            .max(6.0);
        let row_h = ui.text_style_height(&mono);
        let avail = ui.available_size();
        let cols = ((avail.x / char_w).floor() as i32).clamp(2, 5000) as u16;
        let rows = ((avail.y / row_h).floor() as i32).clamp(2, 2000) as u16;

        // Paint terminal grid as monospace lines using LayoutJob
        let desired = egui::vec2(cols as f32 * char_w, rows as f32 * row_h);
        let (rect, resp) = ui.allocate_exact_size(desired, egui::Sense::click());
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(*ui.layout()));
        let text_color = ui.visuals().text_color();
        for row in 0..rows {
            // let y = row as f32 * row_h; // unused
            // Build a layout job for this row
            let mut job = egui::text::LayoutJob::default();
            job.wrap.max_width = f32::INFINITY;
            job.wrap.break_anywhere = false;
            let mut run = String::new();
            let mut run_color = text_color;
            for col in 0..cols {
                let idx = self.term.idx(col, row);
                if idx >= self.term.grid.len() {
                    break;
                }
                let cell = &self.term.grid[idx];
                let c = cell.ch;
                let color = match cell.fg {
                    AnsiColor::Default => text_color,
                    AnsiColor::Index(i) => ansi_index_to_color(i, text_color),
                };
                if color != run_color {
                    if !run.is_empty() {
                        job.append(
                            &run,
                            0.0,
                            egui::TextFormat {
                                color: run_color,
                                ..egui::TextFormat::default()
                            },
                        );
                        run.clear();
                    }
                    run_color = color;
                }
                run.push(c);
            }
            if !run.is_empty() {
                job.append(
                    &run,
                    0.0,
                    egui::TextFormat {
                        color: run_color,
                        ..egui::TextFormat::default()
                    },
                );
            }
            child.add(egui::Label::new(job).truncate());
        }
        if resp.clicked() {
            ui.memory_mut(|m| m.request_focus(resp.id));
        }
        let focused = resp.has_focus();

        self.last_cols = Some(cols);
        self.last_rows = Some(rows);
        (cols, rows, focused)
    }
}

fn ansi_index_to_color(i: u8, default_text: egui::Color32) -> egui::Color32 {
    // Basic 16-color palette: 0-7 normal, 8-15 bright
    match i {
        0 => egui::Color32::BLACK,
        1 => egui::Color32::from_rgb(205, 49, 49),
        2 => egui::Color32::from_rgb(13, 188, 121),
        3 => egui::Color32::from_rgb(229, 229, 16),
        4 => egui::Color32::from_rgb(36, 114, 200),
        5 => egui::Color32::from_rgb(188, 63, 188),
        6 => egui::Color32::from_rgb(17, 168, 205),
        7 => default_text,
        8 => egui::Color32::DARK_GRAY,
        9 => egui::Color32::from_rgb(255, 0, 0),
        10 => egui::Color32::from_rgb(0, 255, 0),
        11 => egui::Color32::from_rgb(255, 255, 0),
        12 => egui::Color32::from_rgb(92, 92, 255),
        13 => egui::Color32::from_rgb(255, 0, 255),
        14 => egui::Color32::from_rgb(0, 255, 255),
        15 => egui::Color32::WHITE,
        _ => default_text,
    }
}
