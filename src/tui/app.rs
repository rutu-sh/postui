use std::cell::Cell;
use std::io;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde::{Deserialize, Serialize};
use ratatui::{
    DefaultTerminal, Frame,
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget, Wrap},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
}

impl HttpMethod {
    const ALL: [HttpMethod; 7] = [
        Self::Get,
        Self::Post,
        Self::Put,
        Self::Patch,
        Self::Delete,
        Self::Head,
        Self::Options,
    ];

    fn as_str(&self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
        }
    }

    fn color(&self) -> Color {
        match self {
            Self::Get => Color::Green,
            Self::Post => Color::Yellow,
            Self::Put => Color::Blue,
            Self::Patch => Color::Magenta,
            Self::Delete => Color::Red,
            Self::Head => Color::Cyan,
            Self::Options => Color::Gray,
        }
    }

    fn allows_body(&self) -> bool {
        matches!(self, Self::Post | Self::Put | Self::Patch | Self::Delete)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Method,
    Url,
    Send,
    Params,
    Response,
    Sidebar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParamsSubFocus {
    Tabs,
    Editor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum RequestTab {
    Params,
    Headers,
    Body,
}

impl RequestTab {
    const ALL: [RequestTab; 3] = [Self::Params, Self::Headers, Self::Body];

    fn label(&self) -> &'static str {
        match self {
            Self::Params => "Params",
            Self::Headers => "Headers",
            Self::Body => "Body",
        }
    }

    fn placeholder(&self) -> &'static str {
        match self {
            Self::Params => "no params",
            Self::Headers => "no headers",
            Self::Body => "raw request body",
        }
    }
}

#[derive(Debug, Default, Clone)]
struct TextBuffer {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
    scroll_y: usize,
}

impl TextBuffer {
    fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            scroll_y: 0,
        }
    }

    fn from_text(text: &str) -> Self {
        let lines: Vec<String> = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(String::from).collect()
        };
        Self {
            lines,
            cursor_row: 0,
            cursor_col: 0,
            scroll_y: 0,
        }
    }

    fn text(&self) -> String {
        self.lines.join("\n")
    }

    fn is_empty(&self) -> bool {
        self.lines.iter().all(|l| l.is_empty())
    }

    fn insert_char(&mut self, c: char) {
        let line = &mut self.lines[self.cursor_row];
        line.insert(self.cursor_col, c);
        self.cursor_col += c.len_utf8();
    }

    fn insert_newline(&mut self) {
        let rest = self.lines[self.cursor_row].split_off(self.cursor_col);
        self.lines.insert(self.cursor_row + 1, rest);
        self.cursor_row += 1;
        self.cursor_col = 0;
    }

    fn backspace(&mut self) {
        if self.cursor_col > 0 {
            let line = &mut self.lines[self.cursor_row];
            let prev = line[..self.cursor_col].chars().next_back().unwrap();
            self.cursor_col -= prev.len_utf8();
            line.remove(self.cursor_col);
        } else if self.cursor_row > 0 {
            let current = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
            self.lines[self.cursor_row].push_str(&current);
        }
    }

    fn delete(&mut self) {
        let line_len = self.lines[self.cursor_row].len();
        if self.cursor_col < line_len {
            self.lines[self.cursor_row].remove(self.cursor_col);
        } else if self.cursor_row + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor_row + 1);
            self.lines[self.cursor_row].push_str(&next);
        }
    }

    fn move_left(&mut self) {
        if self.cursor_col > 0 {
            let prev = self.lines[self.cursor_row][..self.cursor_col]
                .chars()
                .next_back()
                .unwrap();
            self.cursor_col -= prev.len_utf8();
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
        }
    }

    fn move_right(&mut self) {
        let line = &self.lines[self.cursor_row];
        if self.cursor_col < line.len() {
            let next = line[self.cursor_col..].chars().next().unwrap();
            self.cursor_col += next.len_utf8();
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
    }

    fn move_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.clamp_col();
        }
    }

    fn move_down(&mut self) {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.clamp_col();
        }
    }

    fn move_home(&mut self) {
        self.cursor_col = 0;
    }

    fn move_end(&mut self) {
        self.cursor_col = self.lines[self.cursor_row].len();
    }

    fn clamp_col(&mut self) {
        let line = &self.lines[self.cursor_row];
        if self.cursor_col > line.len() {
            self.cursor_col = line.len();
        }
        while self.cursor_col > 0 && !line.is_char_boundary(self.cursor_col) {
            self.cursor_col -= 1;
        }
    }

    fn ensure_visible(&mut self, viewport_height: u16) {
        let vh = viewport_height as usize;
        if vh == 0 {
            return;
        }
        if self.cursor_row < self.scroll_y {
            self.scroll_y = self.cursor_row;
        } else if self.cursor_row >= self.scroll_y + vh {
            self.scroll_y = self.cursor_row + 1 - vh;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KvColumn {
    Enabled,
    Key,
    Value,
}

impl Default for KvColumn {
    fn default() -> Self {
        Self::Key
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct KvRow {
    enabled: bool,
    key: String,
    value: String,
}

impl KvRow {
    fn new() -> Self {
        Self {
            enabled: true,
            key: String::new(),
            value: String::new(),
        }
    }
}

#[derive(Debug, Default, Clone)]
struct KvEditor {
    rows: Vec<KvRow>,
    cur_row: usize,
    cur_col: KvColumn,
    cur_pos: usize,
    scroll_y: usize,
}

impl KvEditor {
    fn new() -> Self {
        Self {
            rows: vec![KvRow::new()],
            cur_row: 0,
            cur_col: KvColumn::Key,
            cur_pos: 0,
            scroll_y: 0,
        }
    }

    fn from_rows(rows: Vec<KvRow>) -> Self {
        let rows = if rows.is_empty() {
            vec![KvRow::new()]
        } else {
            rows
        };
        Self {
            rows,
            cur_row: 0,
            cur_col: KvColumn::Key,
            cur_pos: 0,
            scroll_y: 0,
        }
    }

    fn current_cell(&self) -> &str {
        let row = &self.rows[self.cur_row];
        match self.cur_col {
            KvColumn::Enabled => "",
            KvColumn::Key => &row.key,
            KvColumn::Value => &row.value,
        }
    }

    fn current_cell_mut(&mut self) -> &mut String {
        let row = &mut self.rows[self.cur_row];
        match self.cur_col {
            KvColumn::Enabled => unreachable!("text mutation on Enabled cell"),
            KvColumn::Key => &mut row.key,
            KvColumn::Value => &mut row.value,
        }
    }

    fn is_text_cell(&self) -> bool {
        matches!(self.cur_col, KvColumn::Key | KvColumn::Value)
    }

    fn toggle_enabled(&mut self) {
        let row = &mut self.rows[self.cur_row];
        row.enabled = !row.enabled;
    }

    fn insert_char(&mut self, c: char) {
        if !self.is_text_cell() {
            return;
        }
        let pos = self.cur_pos;
        let cell = self.current_cell_mut();
        cell.insert(pos, c);
        self.cur_pos += c.len_utf8();
    }

    fn backspace(&mut self) {
        if !self.is_text_cell() || self.cur_pos == 0 {
            return;
        }
        let pos = self.cur_pos;
        let cell = self.current_cell_mut();
        let prev = cell[..pos].chars().next_back().unwrap();
        let new_pos = pos - prev.len_utf8();
        cell.remove(new_pos);
        self.cur_pos = new_pos;
    }

    fn delete(&mut self) {
        if !self.is_text_cell() {
            return;
        }
        let pos = self.cur_pos;
        let cell = self.current_cell_mut();
        if pos < cell.len() {
            cell.remove(pos);
        }
    }

    fn move_left(&mut self) {
        match self.cur_col {
            KvColumn::Enabled => {}
            KvColumn::Key => {
                if self.cur_pos > 0 {
                    let prev = self.current_cell()[..self.cur_pos]
                        .chars()
                        .next_back()
                        .unwrap();
                    self.cur_pos -= prev.len_utf8();
                } else {
                    self.cur_col = KvColumn::Enabled;
                    self.cur_pos = 0;
                }
            }
            KvColumn::Value => {
                if self.cur_pos > 0 {
                    let prev = self.current_cell()[..self.cur_pos]
                        .chars()
                        .next_back()
                        .unwrap();
                    self.cur_pos -= prev.len_utf8();
                } else {
                    self.cur_col = KvColumn::Key;
                    self.cur_pos = self.current_cell().len();
                }
            }
        }
    }

    fn move_right(&mut self) {
        match self.cur_col {
            KvColumn::Enabled => {
                self.cur_col = KvColumn::Key;
                self.cur_pos = 0;
            }
            KvColumn::Key => {
                let len = self.current_cell().len();
                if self.cur_pos < len {
                    let next = self.current_cell()[self.cur_pos..].chars().next().unwrap();
                    self.cur_pos += next.len_utf8();
                } else {
                    self.cur_col = KvColumn::Value;
                    self.cur_pos = 0;
                }
            }
            KvColumn::Value => {
                let len = self.current_cell().len();
                if self.cur_pos < len {
                    let next = self.current_cell()[self.cur_pos..].chars().next().unwrap();
                    self.cur_pos += next.len_utf8();
                }
            }
        }
    }

    fn move_up(&mut self) -> bool {
        if self.cur_row > 0 {
            self.cur_row -= 1;
            self.clamp_pos();
            true
        } else {
            false
        }
    }

    fn move_down(&mut self) {
        if self.cur_row + 1 < self.rows.len() {
            self.cur_row += 1;
            self.clamp_pos();
        }
    }

    fn move_home(&mut self) {
        if self.is_text_cell() {
            self.cur_pos = 0;
        }
    }

    fn move_end(&mut self) {
        if self.is_text_cell() {
            self.cur_pos = self.current_cell().len();
        }
    }

    fn clamp_pos(&mut self) {
        if !self.is_text_cell() {
            self.cur_pos = 0;
            return;
        }
        let cell_len = self.current_cell().len();
        if self.cur_pos > cell_len {
            self.cur_pos = cell_len;
        }
        loop {
            if self.cur_pos == 0 || self.current_cell().is_char_boundary(self.cur_pos) {
                break;
            }
            self.cur_pos -= 1;
        }
    }

    fn advance_cell(&mut self) {
        match self.cur_col {
            KvColumn::Enabled => {
                self.cur_col = KvColumn::Key;
                self.cur_pos = 0;
            }
            KvColumn::Key => {
                self.cur_col = KvColumn::Value;
                self.cur_pos = 0;
            }
            KvColumn::Value => {
                if self.cur_row + 1 < self.rows.len() {
                    self.cur_row += 1;
                } else {
                    self.rows.push(KvRow::new());
                    self.cur_row = self.rows.len() - 1;
                }
                self.cur_col = KvColumn::Key;
                self.cur_pos = 0;
            }
        }
    }

    fn retreat_cell(&mut self) {
        match self.cur_col {
            KvColumn::Enabled => {
                if self.cur_row > 0 {
                    self.cur_row -= 1;
                    self.cur_col = KvColumn::Value;
                    self.cur_pos = self.current_cell().len();
                }
            }
            KvColumn::Key => {
                if self.cur_row > 0 {
                    self.cur_row -= 1;
                    self.cur_col = KvColumn::Value;
                    self.cur_pos = self.current_cell().len();
                } else {
                    self.cur_col = KvColumn::Enabled;
                    self.cur_pos = 0;
                }
            }
            KvColumn::Value => {
                self.cur_col = KvColumn::Key;
                self.cur_pos = self.current_cell().len();
            }
        }
    }

    fn delete_current_row(&mut self) {
        if self.rows.len() <= 1 {
            self.rows[0] = KvRow::new();
            self.cur_row = 0;
            self.cur_col = KvColumn::Key;
            self.cur_pos = 0;
            return;
        }
        self.rows.remove(self.cur_row);
        if self.cur_row >= self.rows.len() {
            self.cur_row = self.rows.len() - 1;
        }
        self.cur_col = KvColumn::Key;
        self.clamp_pos();
    }

    fn ensure_visible(&mut self, viewport_height: u16) {
        let vh = viewport_height as usize;
        if vh == 0 {
            return;
        }
        if self.cur_row < self.scroll_y {
            self.scroll_y = self.cur_row;
        } else if self.cur_row >= self.scroll_y + vh {
            self.scroll_y = self.cur_row + 1 - vh;
        }
    }

    fn entries(&self) -> Vec<(String, String)> {
        self.rows
            .iter()
            .filter(|r| r.enabled && !r.key.trim().is_empty())
            .map(|r| (r.key.trim().to_string(), r.value.trim().to_string()))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResponseDisplay {
    status: u16,
    status_text: String,
    body: String,
    elapsed_ms: u128,
}

#[derive(Debug)]
enum ResponseState {
    Empty,
    InFlight,
    Done(ResponseDisplay),
    Error(String),
}

#[derive(Debug)]
pub struct App {
    name: String,
    exit: bool,
    show_header: bool,
    show_sidebar: bool,
    show_footer: bool,
    focus: Focus,
    method: HttpMethod,
    method_dropdown_open: bool,
    method_dropdown_index: usize,
    method_area: Cell<Rect>,
    url: String,
    url_cursor: usize,
    url_area: Cell<Rect>,
    send_area: Cell<Rect>,
    url_row_area: Cell<Rect>,
    active_tab: RequestTab,
    params_sub_focus: ParamsSubFocus,
    params_kv: KvEditor,
    headers_kv: KvEditor,
    body_buf: TextBuffer,
    editor_area: Cell<Rect>,
    response: ResponseState,
    pending_response: Option<mpsc::Receiver<Result<ResponseDisplay, String>>>,
    response_scroll: u16,
    response_area: Cell<Rect>,
    response_max_scroll: Cell<u16>,
    split_ratio: u16,
    status_message: Option<(String, Instant)>,
    requests: Vec<SavedRequest>,
    sidebar_cursor: SidebarCursor,
    sidebar_area: Cell<Rect>,
    current_request_idx: Option<usize>,
    renaming: Option<RenameState>,
    rename_input_area: Cell<Rect>,
    path_input: Option<PathInputState>,
    footer_area: Cell<Rect>,
}

#[derive(Debug)]
struct RenameState {
    target: usize,
    text: String,
    cursor: usize,
}

#[derive(Debug)]
struct PathInputState {
    text: String,
    cursor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidebarCursor {
    NewRequest,
    Saved(usize),
}

impl SidebarCursor {
    fn saved_index(&self) -> Option<usize> {
        match self {
            Self::Saved(i) => Some(*i),
            _ => None,
        }
    }
}

impl Default for App {
    fn default() -> Self {
        let url = String::new();
        let url_cursor = 0;
        Self {
            name: "postui".into(),
            exit: false,
            show_header: true,
            show_sidebar: true,
            show_footer: true,
            focus: Focus::Method,
            method: HttpMethod::Get,
            method_dropdown_open: false,
            method_dropdown_index: 0,
            method_area: Cell::new(Rect::default()),
            url,
            url_cursor,
            url_area: Cell::new(Rect::default()),
            send_area: Cell::new(Rect::default()),
            url_row_area: Cell::new(Rect::default()),
            active_tab: RequestTab::Params,
            params_sub_focus: ParamsSubFocus::Tabs,
            params_kv: KvEditor::new(),
            headers_kv: KvEditor::new(),
            body_buf: TextBuffer::new(),
            editor_area: Cell::new(Rect::default()),
            response: ResponseState::Empty,
            pending_response: None,
            response_scroll: 0,
            response_area: Cell::new(Rect::default()),
            response_max_scroll: Cell::new(0),
            split_ratio: 50,
            status_message: None,
            requests: load_collection_from_disk(),
            sidebar_cursor: SidebarCursor::NewRequest,
            sidebar_area: Cell::new(Rect::default()),
            current_request_idx: None,
            renaming: None,
            rename_input_area: Cell::new(Rect::default()),
            path_input: None,
            footer_area: Cell::new(Rect::default()),
        }
    }
}

impl App {
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        while !self.exit {
            terminal.draw(|frame| self.draw(frame))?;
            self.handle_events()?;
            self.poll_response();
        }
        Ok(())
    }

    fn draw(&self, frame: &mut Frame) {
        frame.render_widget(self, frame.area());
        if self.method_dropdown_open {
            self.render_method_dropdown(frame);
        } else if self.path_input.is_some() {
            self.render_path_input_cursor(frame);
        } else if self.focus == Focus::Url {
            self.render_url_cursor(frame);
        } else if self.focus == Focus::Params
            && self.params_sub_focus == ParamsSubFocus::Editor
        {
            self.render_editor_cursor(frame);
        } else if self.focus == Focus::Sidebar && self.renaming.is_some() {
            self.render_rename_cursor(frame);
        }
    }

    fn handle_events(&mut self) -> io::Result<()> {
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? && key.kind == KeyEventKind::Press {
                self.handle_key(key);
            }
        }
        Ok(())
    }

    fn poll_response(&mut self) {
        if let Some(rx) = self.pending_response.as_ref() {
            match rx.try_recv() {
                Ok(Ok(disp)) => {
                    self.response = ResponseState::Done(disp);
                    self.response_scroll = 0;
                    self.pending_response = None;
                }
                Ok(Err(msg)) => {
                    self.response = ResponseState::Error(msg);
                    self.response_scroll = 0;
                    self.pending_response = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    if matches!(self.response, ResponseState::InFlight) {
                        self.response = ResponseState::Error("worker disconnected".into());
                    }
                    self.pending_response = None;
                }
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.exit();
            return;
        }

        if self.path_input.is_some() {
            self.handle_path_input_key(key);
            return;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => {
                    self.exit();
                    return;
                }
                KeyCode::Char('s') => {
                    self.save_state();
                    return;
                }
                KeyCode::Char('o') => {
                    self.start_load_path_input();
                    return;
                }
                KeyCode::Char('b') => {
                    self.toggle_sidebar_focus();
                    return;
                }
                _ => {}
            }
        }

        if key.modifiers.contains(KeyModifiers::SHIFT) {
            match key.code {
                KeyCode::Up => {
                    self.split_ratio = self.split_ratio.saturating_sub(5).max(15);
                    return;
                }
                KeyCode::Down => {
                    self.split_ratio = (self.split_ratio + 5).min(85);
                    return;
                }
                _ => {}
            }
        }

        if self.method_dropdown_open {
            match key.code {
                KeyCode::Esc => self.method_dropdown_open = false,
                KeyCode::Up => {
                    self.method_dropdown_index = if self.method_dropdown_index == 0 {
                        HttpMethod::ALL.len() - 1
                    } else {
                        self.method_dropdown_index - 1
                    };
                }
                KeyCode::Down => {
                    self.method_dropdown_index =
                        (self.method_dropdown_index + 1) % HttpMethod::ALL.len();
                }
                KeyCode::Enter => {
                    self.method = HttpMethod::ALL[self.method_dropdown_index];
                    self.method_dropdown_open = false;
                }
                _ => {}
            }
            return;
        }

        match self.focus {
            Focus::Url => self.handle_url_key(key),
            Focus::Params => self.handle_params_key(key),
            Focus::Response => self.handle_response_key(key),
            Focus::Sidebar => self.handle_sidebar_key(key),
            Focus::Method | Focus::Send => self.handle_button_key(key),
        }
    }

    fn handle_button_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.exit(),
            KeyCode::Char('h') => self.show_header = !self.show_header,
            KeyCode::Char('s') => {
                self.show_sidebar = !self.show_sidebar;
                if !self.show_sidebar && self.focus == Focus::Sidebar {
                    self.focus = Focus::Method;
                }
            }
            KeyCode::Char('f') => self.show_footer = !self.show_footer,
            KeyCode::Tab => self.cycle_focus(),
            KeyCode::BackTab => self.cycle_focus_back(),
            KeyCode::Enter | KeyCode::Down if self.focus == Focus::Method => {
                self.open_method_dropdown();
            }
            KeyCode::Enter | KeyCode::Char(' ') if self.focus == Focus::Send => {
                self.send_request();
            }
            _ => {}
        }
    }

    fn handle_url_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Tab => self.cycle_focus(),
            KeyCode::BackTab => self.cycle_focus_back(),
            KeyCode::Enter => self.send_request(),
            KeyCode::Left => {
                if self.url_cursor > 0 {
                    let prev = self.url[..self.url_cursor].chars().next_back().unwrap();
                    self.url_cursor -= prev.len_utf8();
                }
            }
            KeyCode::Right => {
                if self.url_cursor < self.url.len() {
                    let next = self.url[self.url_cursor..].chars().next().unwrap();
                    self.url_cursor += next.len_utf8();
                }
            }
            KeyCode::Home => self.url_cursor = 0,
            KeyCode::End => self.url_cursor = self.url.len(),
            KeyCode::Backspace => {
                if self.url_cursor > 0 {
                    let prev = self.url[..self.url_cursor].chars().next_back().unwrap();
                    self.url_cursor -= prev.len_utf8();
                    self.url.remove(self.url_cursor);
                }
            }
            KeyCode::Delete => {
                if self.url_cursor < self.url.len() {
                    self.url.remove(self.url_cursor);
                }
            }
            KeyCode::Char(c) => {
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                    self.url.insert(self.url_cursor, c);
                    self.url_cursor += c.len_utf8();
                }
            }
            _ => {}
        }
    }

    fn handle_params_key(&mut self, key: KeyEvent) {
        let in_editor = self.params_sub_focus == ParamsSubFocus::Editor;
        let in_kv_editor = in_editor && self.active_tab != RequestTab::Body;

        match key.code {
            KeyCode::Tab => {
                if !in_editor {
                    self.cycle_focus();
                    return;
                }
                // editor handles Tab (KV: advance cell, Body: insert spaces)
            }
            KeyCode::BackTab => {
                if !in_kv_editor {
                    self.cycle_focus_back();
                    return;
                }
                // KV editor handles Shift+Tab (retreat cell)
            }
            KeyCode::Esc => {
                if self.params_sub_focus == ParamsSubFocus::Editor {
                    self.params_sub_focus = ParamsSubFocus::Tabs;
                }
                return;
            }
            _ => {}
        }

        match self.params_sub_focus {
            ParamsSubFocus::Tabs => self.handle_params_tabs_key(key),
            ParamsSubFocus::Editor => self.handle_editor_key(key),
        }
    }

    fn handle_params_tabs_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Left => self.prev_tab(),
            KeyCode::Right => self.next_tab(),
            KeyCode::Down | KeyCode::Enter => {
                self.params_sub_focus = ParamsSubFocus::Editor;
            }
            _ => {}
        }
    }

    fn handle_editor_key(&mut self, key: KeyEvent) {
        match self.active_tab {
            RequestTab::Params | RequestTab::Headers => self.handle_kv_editor_key(key),
            RequestTab::Body => self.handle_body_editor_key(key),
        }
    }

    fn handle_kv_editor_key(&mut self, key: KeyEvent) {
        let editor_h = self.editor_area.get().height;
        let kv_viewport = editor_h.saturating_sub(2);

        if key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::CONTROL) {
            let ed = self.active_kv_editor_mut();
            ed.delete_current_row();
            ed.ensure_visible(kv_viewport);
            return;
        }

        if matches!(key.code, KeyCode::Up) && self.active_kv_editor().cur_row == 0 {
            self.params_sub_focus = ParamsSubFocus::Tabs;
            return;
        }

        let ed = self.active_kv_editor_mut();
        match key.code {
            KeyCode::Up => {
                ed.move_up();
            }
            KeyCode::Down => ed.move_down(),
            KeyCode::Left => ed.move_left(),
            KeyCode::Right => ed.move_right(),
            KeyCode::Tab => ed.advance_cell(),
            KeyCode::BackTab => ed.retreat_cell(),
            KeyCode::Home => ed.move_home(),
            KeyCode::End => ed.move_end(),
            KeyCode::PageUp => {
                let page = kv_viewport.max(1);
                for _ in 0..page {
                    if !ed.move_up() {
                        break;
                    }
                }
            }
            KeyCode::PageDown => {
                let page = kv_viewport.max(1);
                for _ in 0..page {
                    ed.move_down();
                }
            }
            KeyCode::Enter => {
                if ed.cur_col == KvColumn::Enabled {
                    ed.toggle_enabled();
                } else {
                    ed.advance_cell();
                }
            }
            KeyCode::Char(' ') if ed.cur_col == KvColumn::Enabled => {
                ed.toggle_enabled();
            }
            KeyCode::Backspace => ed.backspace(),
            KeyCode::Delete => ed.delete(),
            KeyCode::Char(c) => {
                if (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
                    && ed.is_text_cell()
                {
                    ed.insert_char(c);
                }
            }
            _ => {}
        }
        ed.ensure_visible(kv_viewport);
    }

    fn handle_body_editor_key(&mut self, key: KeyEvent) {
        let viewport_height = self.editor_area.get().height;
        if key.code == KeyCode::Up && self.body_buf.cursor_row == 0 {
            self.params_sub_focus = ParamsSubFocus::Tabs;
            return;
        }
        match key.code {
            KeyCode::Up => self.body_buf.move_up(),
            KeyCode::Down => self.body_buf.move_down(),
            KeyCode::Left => self.body_buf.move_left(),
            KeyCode::Right => self.body_buf.move_right(),
            KeyCode::Home => self.body_buf.move_home(),
            KeyCode::End => self.body_buf.move_end(),
            KeyCode::PageUp => {
                let page = (viewport_height / 2).max(1);
                for _ in 0..page {
                    self.body_buf.move_up();
                }
            }
            KeyCode::PageDown => {
                let page = (viewport_height / 2).max(1);
                for _ in 0..page {
                    self.body_buf.move_down();
                }
            }
            KeyCode::Enter => self.body_buf.insert_newline(),
            KeyCode::Backspace => self.body_buf.backspace(),
            KeyCode::Delete => self.body_buf.delete(),
            KeyCode::Tab => {
                self.body_buf.insert_char(' ');
                self.body_buf.insert_char(' ');
            }
            KeyCode::Char(c) => {
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                    self.body_buf.insert_char(c);
                }
            }
            _ => {}
        }
        self.body_buf.ensure_visible(viewport_height);
    }

    fn handle_response_key(&mut self, key: KeyEvent) {
        let max = self.response_max_scroll.get();
        let page = (self.response_area.get().height / 2).max(1);
        match key.code {
            KeyCode::Tab => self.cycle_focus(),
            KeyCode::BackTab => self.cycle_focus_back(),
            KeyCode::Up | KeyCode::Char('k') => {
                self.response_scroll = self.response_scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.response_scroll = (self.response_scroll + 1).min(max);
            }
            KeyCode::PageUp => {
                self.response_scroll = self.response_scroll.saturating_sub(page);
            }
            KeyCode::PageDown => {
                self.response_scroll = (self.response_scroll + page).min(max);
            }
            KeyCode::Home | KeyCode::Char('g') => self.response_scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.response_scroll = max,
            KeyCode::Char('c') | KeyCode::Char('y') => self.copy_response(),
            _ => {}
        }
    }

    fn copy_response(&mut self) {
        let body = match &self.response {
            ResponseState::Done(d) => d.body.clone(),
            ResponseState::Error(e) => e.clone(),
            _ => {
                self.set_status("nothing to copy".into());
                return;
            }
        };
        match copy_to_clipboard(&body) {
            Ok(()) => self.set_status(format!("copied {} bytes", body.len())),
            Err(e) => self.set_status(format!("copy failed — {e}")),
        }
    }

    fn handle_sidebar_key(&mut self, key: KeyEvent) {
        if self.renaming.is_some() {
            self.handle_rename_key(key);
            return;
        }
        match key.code {
            KeyCode::Tab => self.cycle_focus(),
            KeyCode::BackTab => self.cycle_focus_back(),
            KeyCode::Up | KeyCode::Char('k') => {
                self.sidebar_cursor = match self.sidebar_cursor {
                    SidebarCursor::NewRequest => SidebarCursor::NewRequest,
                    SidebarCursor::Saved(0) => SidebarCursor::NewRequest,
                    SidebarCursor::Saved(i) => SidebarCursor::Saved(i - 1),
                };
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.sidebar_cursor = match self.sidebar_cursor {
                    SidebarCursor::NewRequest => {
                        if self.requests.is_empty() {
                            SidebarCursor::NewRequest
                        } else {
                            SidebarCursor::Saved(0)
                        }
                    }
                    SidebarCursor::Saved(i) => {
                        if i + 1 < self.requests.len() {
                            SidebarCursor::Saved(i + 1)
                        } else {
                            SidebarCursor::Saved(i)
                        }
                    }
                };
            }
            KeyCode::Home => self.sidebar_cursor = SidebarCursor::NewRequest,
            KeyCode::End => {
                if !self.requests.is_empty() {
                    self.sidebar_cursor = SidebarCursor::Saved(self.requests.len() - 1);
                }
            }
            KeyCode::Enter => match self.sidebar_cursor {
                SidebarCursor::NewRequest => self.new_request(),
                SidebarCursor::Saved(_) => self.load_selected_request(),
            },
            KeyCode::Char('d') => {
                if matches!(self.sidebar_cursor, SidebarCursor::Saved(_)) {
                    self.delete_selected_request();
                }
            }
            KeyCode::Char('r') => {
                if matches!(self.sidebar_cursor, SidebarCursor::Saved(_)) {
                    self.start_rename();
                }
            }
            _ => {}
        }
    }

    fn start_rename(&mut self) {
        let Some(idx) = self.sidebar_cursor.saved_index() else {
            return;
        };
        if let Some(req) = self.requests.get(idx) {
            let initial = req.name.clone().unwrap_or_default();
            let cursor = initial.len();
            self.renaming = Some(RenameState {
                target: idx,
                text: initial,
                cursor,
            });
        }
    }

    fn handle_rename_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.renaming = None;
            }
            KeyCode::Enter => {
                if let Some(rename) = self.renaming.take() {
                    let name = if rename.text.trim().is_empty() {
                        None
                    } else {
                        Some(rename.text.trim().to_string())
                    };
                    if let Some(req) = self.requests.get_mut(rename.target) {
                        req.name = name;
                    }
                    match self.persist_collection() {
                        Ok(()) => self.set_status("renamed".into()),
                        Err(e) => self.set_status(format!("rename failed — {e}")),
                    }
                }
            }
            KeyCode::Left => {
                if let Some(rename) = self.renaming.as_mut() {
                    if rename.cursor > 0 {
                        let prev = rename.text[..rename.cursor]
                            .chars()
                            .next_back()
                            .unwrap();
                        rename.cursor -= prev.len_utf8();
                    }
                }
            }
            KeyCode::Right => {
                if let Some(rename) = self.renaming.as_mut() {
                    if rename.cursor < rename.text.len() {
                        let next = rename.text[rename.cursor..].chars().next().unwrap();
                        rename.cursor += next.len_utf8();
                    }
                }
            }
            KeyCode::Home => {
                if let Some(rename) = self.renaming.as_mut() {
                    rename.cursor = 0;
                }
            }
            KeyCode::End => {
                if let Some(rename) = self.renaming.as_mut() {
                    rename.cursor = rename.text.len();
                }
            }
            KeyCode::Backspace => {
                if let Some(rename) = self.renaming.as_mut() {
                    if rename.cursor > 0 {
                        let prev = rename.text[..rename.cursor]
                            .chars()
                            .next_back()
                            .unwrap();
                        rename.cursor -= prev.len_utf8();
                        rename.text.remove(rename.cursor);
                    }
                }
            }
            KeyCode::Delete => {
                if let Some(rename) = self.renaming.as_mut() {
                    if rename.cursor < rename.text.len() {
                        rename.text.remove(rename.cursor);
                    }
                }
            }
            KeyCode::Char(c) => {
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                    if let Some(rename) = self.renaming.as_mut() {
                        rename.text.insert(rename.cursor, c);
                        rename.cursor += c.len_utf8();
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_path_input_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.path_input = None;
            }
            KeyCode::Enter => {
                if let Some(input) = self.path_input.take() {
                    self.reload_collection_from(&input.text);
                }
            }
            KeyCode::Left => {
                if let Some(input) = self.path_input.as_mut() {
                    if input.cursor > 0 {
                        let prev = input.text[..input.cursor].chars().next_back().unwrap();
                        input.cursor -= prev.len_utf8();
                    }
                }
            }
            KeyCode::Right => {
                if let Some(input) = self.path_input.as_mut() {
                    if input.cursor < input.text.len() {
                        let next = input.text[input.cursor..].chars().next().unwrap();
                        input.cursor += next.len_utf8();
                    }
                }
            }
            KeyCode::Home => {
                if let Some(input) = self.path_input.as_mut() {
                    input.cursor = 0;
                }
            }
            KeyCode::End => {
                if let Some(input) = self.path_input.as_mut() {
                    input.cursor = input.text.len();
                }
            }
            KeyCode::Backspace => {
                if let Some(input) = self.path_input.as_mut() {
                    if input.cursor > 0 {
                        let prev = input.text[..input.cursor].chars().next_back().unwrap();
                        input.cursor -= prev.len_utf8();
                        input.text.remove(input.cursor);
                    }
                }
            }
            KeyCode::Delete => {
                if let Some(input) = self.path_input.as_mut() {
                    if input.cursor < input.text.len() {
                        input.text.remove(input.cursor);
                    }
                }
            }
            KeyCode::Char(c) => {
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                    if let Some(input) = self.path_input.as_mut() {
                        input.text.insert(input.cursor, c);
                        input.cursor += c.len_utf8();
                    }
                }
            }
            _ => {}
        }
    }

    fn toggle_sidebar_focus(&mut self) {
        if !self.show_sidebar {
            self.show_sidebar = true;
        }
        self.focus = if self.focus == Focus::Sidebar {
            Focus::Method
        } else {
            Focus::Sidebar
        };
        self.params_sub_focus = ParamsSubFocus::Tabs;
    }

    fn prev_tab(&mut self) {
        let idx = RequestTab::ALL
            .iter()
            .position(|t| *t == self.active_tab)
            .unwrap_or(0);
        let new_idx = if idx == 0 { RequestTab::ALL.len() - 1 } else { idx - 1 };
        self.active_tab = RequestTab::ALL[new_idx];
    }

    fn next_tab(&mut self) {
        let idx = RequestTab::ALL
            .iter()
            .position(|t| *t == self.active_tab)
            .unwrap_or(0);
        self.active_tab = RequestTab::ALL[(idx + 1) % RequestTab::ALL.len()];
    }

    fn active_kv_editor(&self) -> &KvEditor {
        match self.active_tab {
            RequestTab::Params => &self.params_kv,
            RequestTab::Headers => &self.headers_kv,
            RequestTab::Body => unreachable!("kv editor on Body tab"),
        }
    }

    fn active_kv_editor_mut(&mut self) -> &mut KvEditor {
        match self.active_tab {
            RequestTab::Params => &mut self.params_kv,
            RequestTab::Headers => &mut self.headers_kv,
            RequestTab::Body => unreachable!("kv editor on Body tab"),
        }
    }

    fn cycle_focus(&mut self) {
        match self.focus {
            Focus::Sidebar => {
                self.focus = Focus::Method;
                self.params_sub_focus = ParamsSubFocus::Tabs;
            }
            Focus::Method => {
                self.focus = Focus::Url;
                self.params_sub_focus = ParamsSubFocus::Tabs;
            }
            Focus::Url => {
                self.focus = Focus::Send;
                self.params_sub_focus = ParamsSubFocus::Tabs;
            }
            Focus::Send => {
                self.focus = Focus::Params;
                self.active_tab = RequestTab::Params;
                self.params_sub_focus = ParamsSubFocus::Tabs;
            }
            Focus::Params => match self.active_tab {
                RequestTab::Params => self.active_tab = RequestTab::Headers,
                RequestTab::Headers => self.active_tab = RequestTab::Body,
                RequestTab::Body => {
                    self.focus = Focus::Response;
                    self.params_sub_focus = ParamsSubFocus::Tabs;
                }
            },
            Focus::Response => {
                self.focus = if self.show_sidebar {
                    Focus::Sidebar
                } else {
                    Focus::Method
                };
                self.params_sub_focus = ParamsSubFocus::Tabs;
            }
        }
    }

    fn cycle_focus_back(&mut self) {
        match self.focus {
            Focus::Sidebar => {
                self.focus = Focus::Response;
                self.params_sub_focus = ParamsSubFocus::Tabs;
            }
            Focus::Method => {
                self.focus = if self.show_sidebar {
                    Focus::Sidebar
                } else {
                    Focus::Response
                };
                self.params_sub_focus = ParamsSubFocus::Tabs;
            }
            Focus::Url => {
                self.focus = Focus::Method;
                self.params_sub_focus = ParamsSubFocus::Tabs;
            }
            Focus::Send => {
                self.focus = Focus::Url;
                self.params_sub_focus = ParamsSubFocus::Tabs;
            }
            Focus::Params => match self.active_tab {
                RequestTab::Body => self.active_tab = RequestTab::Headers,
                RequestTab::Headers => self.active_tab = RequestTab::Params,
                RequestTab::Params => {
                    self.focus = Focus::Send;
                    self.params_sub_focus = ParamsSubFocus::Tabs;
                }
            },
            Focus::Response => {
                self.focus = Focus::Params;
                self.active_tab = RequestTab::Body;
                self.params_sub_focus = ParamsSubFocus::Tabs;
            }
        }
    }

    fn open_method_dropdown(&mut self) {
        self.method_dropdown_open = true;
        self.method_dropdown_index = HttpMethod::ALL
            .iter()
            .position(|m| *m == self.method)
            .unwrap_or(0);
    }

    fn send_request(&mut self) {
        if matches!(self.response, ResponseState::InFlight) {
            return;
        }
        if self.url.trim().is_empty() {
            self.response = ResponseState::Error("URL is empty".into());
            return;
        }

        let (tx, rx) = mpsc::channel();
        let url = self.url.clone();
        let method = self.method.as_str().to_string();
        let params = self.params_kv.entries();
        let headers = self.headers_kv.entries();
        let body = self.body_buf.text();
        let allow_body = self.method.allows_body();

        self.response = ResponseState::InFlight;
        self.pending_response = Some(rx);

        thread::spawn(move || {
            let agent = ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .build();
            let mut req = agent.request(&method, &url);
            for (k, v) in &params {
                req = req.query(k, v);
            }
            for (k, v) in &headers {
                req = req.set(k, v);
            }
            let start = Instant::now();
            let response = if allow_body && !body.is_empty() {
                req.send_string(&body)
            } else {
                req.call()
            };
            let elapsed_ms = start.elapsed().as_millis();
            let result = match response {
                Ok(resp) => {
                    let status = resp.status();
                    let status_text = resp.status_text().to_string();
                    let body = resp
                        .into_string()
                        .unwrap_or_else(|e| format!("(body read error: {e})"));
                    Ok(ResponseDisplay { status, status_text, body, elapsed_ms })
                }
                Err(ureq::Error::Status(code, resp)) => {
                    let status_text = resp.status_text().to_string();
                    let body = resp
                        .into_string()
                        .unwrap_or_else(|e| format!("(body read error: {e})"));
                    Ok(ResponseDisplay { status: code, status_text, body, elapsed_ms })
                }
                Err(ureq::Error::Transport(t)) => Err(format!("{t}")),
            };
            let _ = tx.send(result);
        });
    }

    fn exit(&mut self) {
        self.exit = true;
    }

    fn is_current_dirty(&self) -> bool {
        let Some(idx) = self.current_request_idx else {
            return false;
        };
        let Some(saved) = self.requests.get(idx) else {
            return false;
        };
        saved.method != self.method
            || saved.url != self.url
            || !kv_rows_equivalent(&saved.params, &self.params_kv.rows)
            || !kv_rows_equivalent(&saved.headers, &self.headers_kv.rows)
            || saved.body != self.body_buf.text()
    }

    fn set_status(&mut self, msg: String) {
        self.status_message = Some((msg, Instant::now()));
    }

    fn save_state(&mut self) {
        let snapshot = SavedRequest {
            name: None,
            method: self.method,
            url: self.url.clone(),
            params: self.params_kv.rows.clone(),
            headers: self.headers_kv.rows.clone(),
            body: self.body_buf.text(),
            last_response: match &self.response {
                ResponseState::Done(d) => Some(d.clone()),
                _ => None,
            },
        };

        if let Some(idx) = self.current_request_idx {
            if idx < self.requests.len() {
                let preserved_name = self.requests[idx].name.clone();
                let updated = SavedRequest { name: preserved_name, ..snapshot };
                let prev = std::mem::replace(&mut self.requests[idx], updated);
                match self.persist_collection() {
                    Ok(()) => self.set_status(format!("updated entry {}", idx + 1)),
                    Err(e) => {
                        self.requests[idx] = prev;
                        self.set_status(format!("save failed — {e}"));
                    }
                }
                return;
            }
        }

        self.requests.push(snapshot);
        let new_idx = self.requests.len() - 1;
        self.sidebar_cursor = SidebarCursor::Saved(new_idx);
        match self.persist_collection() {
            Ok(()) => {
                self.current_request_idx = Some(new_idx);
                self.set_status(format!("saved ({} total)", self.requests.len()));
            }
            Err(e) => {
                self.requests.pop();
                if self.requests.is_empty() {
                    self.sidebar_cursor = SidebarCursor::NewRequest;
                } else {
                    self.sidebar_cursor = SidebarCursor::Saved(self.requests.len() - 1);
                }
                self.set_status(format!("save failed — {e}"));
            }
        }
    }

    fn start_load_path_input(&mut self) {
        let initial = collection_file_path().to_string_lossy().to_string();
        let cursor = initial.len();
        self.path_input = Some(PathInputState { text: initial, cursor });
    }

    fn reload_collection_from(&mut self, path_str: &str) {
        let expanded = expand_tilde(path_str);
        let path = std::path::Path::new(&expanded);
        let result = std::fs::read_to_string(path)
            .map_err(|e| format!("read: {e}"))
            .and_then(|s| {
                serde_json::from_str::<Vec<SavedRequest>>(&s).map_err(|e| format!("parse: {e}"))
            });
        match result {
            Ok(loaded) => {
                self.requests = loaded;
                if self.requests.is_empty() {
                    self.sidebar_cursor = SidebarCursor::NewRequest;
                    self.current_request_idx = None;
                } else {
                    if let SidebarCursor::Saved(i) = self.sidebar_cursor {
                        if i >= self.requests.len() {
                            self.sidebar_cursor = SidebarCursor::Saved(self.requests.len() - 1);
                        }
                    }
                    if let Some(idx) = self.current_request_idx {
                        if idx >= self.requests.len() {
                            self.current_request_idx = None;
                        }
                    }
                }
                self.renaming = None;
                self.set_status(format!(
                    "loaded {} requests from {}",
                    self.requests.len(),
                    expanded
                ));
            }
            Err(e) => self.set_status(format!("load failed — {e}")),
        }
    }

    fn persist_collection(&self) -> Result<(), String> {
        let path = collection_file_path();
        let json = serde_json::to_string_pretty(&self.requests)
            .map_err(|e| format!("serialize: {e}"))?;
        std::fs::write(&path, json).map_err(|e| format!("write: {e}"))
    }

    fn load_selected_request(&mut self) {
        let Some(idx) = self.sidebar_cursor.saved_index() else {
            return;
        };
        let Some(req) = self.requests.get(idx).cloned() else {
            return;
        };
        let label_target = req
            .name
            .clone()
            .unwrap_or_else(|| truncate_for_display(&req.url, 40));
        let label = format!("loaded {} {}", req.method.as_str(), label_target);
        self.method = req.method;
        self.url = req.url;
        self.url_cursor = self.url.len();
        self.params_kv = KvEditor::from_rows(req.params);
        self.headers_kv = KvEditor::from_rows(req.headers);
        self.body_buf = TextBuffer::from_text(&req.body);
        self.response = match req.last_response {
            Some(d) => ResponseState::Done(d),
            None => ResponseState::Empty,
        };
        self.response_scroll = 0;
        self.pending_response = None;
        self.current_request_idx = Some(idx);
        self.set_status(label);
    }

    fn delete_selected_request(&mut self) {
        let Some(idx) = self.sidebar_cursor.saved_index() else {
            return;
        };
        if idx >= self.requests.len() {
            return;
        }
        let removed = self.requests.remove(idx);
        if self.requests.is_empty() {
            self.sidebar_cursor = SidebarCursor::NewRequest;
        } else if idx >= self.requests.len() {
            self.sidebar_cursor = SidebarCursor::Saved(self.requests.len() - 1);
        }
        self.current_request_idx = match self.current_request_idx {
            Some(curr) if curr == idx => None,
            Some(curr) if curr > idx => Some(curr - 1),
            other => other,
        };
        if let Some(rs) = &self.renaming {
            if rs.target == idx {
                self.renaming = None;
            }
        }
        let _ = self.persist_collection();
        let display_target = removed
            .name
            .clone()
            .unwrap_or_else(|| truncate_for_display(&removed.url, 40));
        self.set_status(format!("deleted {} {}", removed.method.as_str(), display_target));
    }

    fn new_request(&mut self) {
        self.method = HttpMethod::Get;
        self.url = String::new();
        self.url_cursor = 0;
        self.params_kv = KvEditor::new();
        self.headers_kv = KvEditor::new();
        self.body_buf = TextBuffer::new();
        self.response = ResponseState::Empty;
        self.response_scroll = 0;
        self.pending_response = None;
        self.current_request_idx = None;
        self.method_dropdown_open = false;
        self.renaming = None;
        self.focus = Focus::Url;
        self.params_sub_focus = ParamsSubFocus::Tabs;
        self.active_tab = RequestTab::Params;
        self.set_status("new request".into());
    }

    fn render_sidebar(&self, area: Rect, buf: &mut Buffer) {
        self.sidebar_area.set(area);
        let focused = self.focus == Focus::Sidebar;
        let border_color = if focused { Color::LightCyan } else { Color::Gray };
        Block::new()
            .borders(Borders::TOP | Borders::LEFT | Borders::BOTTOM)
            .border_style(Style::default().fg(border_color))
            .render(area, buf);

        if area.width < 4 || area.height < 2 {
            return;
        }
        let inner = Rect {
            x: area.x + 1,
            y: area.y + 1,
            width: area.width.saturating_sub(1),
            height: area.height.saturating_sub(2),
        };
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let title_style = Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD);
        Paragraph::new(Line::from(Span::styled(" Saved Requests", title_style))).render(
            Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 },
            buf,
        );

        if inner.height < 2 {
            return;
        }

        let list_area = Rect {
            x: inner.x,
            y: inner.y + 1,
            width: inner.width,
            height: inner.height - 1,
        };

        let max_items = list_area.height as usize;
        if max_items == 0 {
            return;
        }

        let total_rows = 1 + self.requests.len();
        let cursor_row: usize = match self.sidebar_cursor {
            SidebarCursor::NewRequest => 0,
            SidebarCursor::Saved(i) => 1 + i,
        };
        let scroll = if cursor_row >= max_items {
            cursor_row + 1 - max_items
        } else {
            0
        };

        let dirty_idx: Option<usize> = if self.is_current_dirty() {
            self.current_request_idx
        } else {
            None
        };

        let method_w: u16 = 8;
        for display_i in 0..max_items {
            let logical_row = scroll + display_i;
            if logical_row >= total_rows {
                break;
            }
            let y = list_area.y + display_i as u16;
            let row_area = Rect {
                x: list_area.x,
                y,
                width: list_area.width,
                height: 1,
            };

            if logical_row == 0 {
                let is_selected = matches!(self.sidebar_cursor, SidebarCursor::NewRequest);
                if is_selected {
                    let bg = if focused { Color::DarkGray } else { Color::Black };
                    Block::new().bg(bg).render(row_area, buf);
                }
                let style = Style::default()
                    .fg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD);
                Paragraph::new(Line::from(Span::styled(" + New Request", style)))
                    .render(row_area, buf);
                continue;
            }

            let i = logical_row - 1;
            let req = &self.requests[i];
            let is_selected = self.sidebar_cursor == SidebarCursor::Saved(i);
            let is_current = self.current_request_idx == Some(i);
            let renaming_here = self
                .renaming
                .as_ref()
                .map(|r| r.target == i)
                .unwrap_or(false);

            if is_selected {
                let bg = if focused { Color::DarkGray } else { Color::Black };
                Block::new().bg(bg).render(row_area, buf);
            }

            if renaming_here {
                self.rename_input_area.set(row_area);
                let rename = self.renaming.as_ref().unwrap();
                Block::new().bg(Color::Blue).render(row_area, buf);
                let visible_w = row_area.width.saturating_sub(1) as usize;
                let (display, style) = if rename.text.is_empty() {
                    (
                        "(name)".to_string(),
                        Style::default()
                            .fg(Color::Gray)
                            .bg(Color::Blue)
                            .add_modifier(Modifier::ITALIC),
                    )
                } else {
                    (
                        rename.text.chars().take(visible_w).collect(),
                        Style::default()
                            .fg(Color::White)
                            .bg(Color::Blue)
                            .add_modifier(Modifier::BOLD),
                    )
                };
                let line = Line::from(vec![Span::raw(" "), Span::styled(display, style)]);
                Paragraph::new(line).render(row_area, buf);
                continue;
            }

            let is_dirty = dirty_idx == Some(i);
            let label = req.name.clone().unwrap_or_else(|| req.url.clone());
            let marker = if is_dirty {
                "*"
            } else if is_current {
                "●"
            } else {
                " "
            };
            let marker_style = if is_dirty {
                Style::default()
                    .fg(Color::LightRed)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD)
            };
            let label_w = row_area.width.saturating_sub(method_w + 3) as usize;
            let label_disp = if label_w == 0 {
                String::new()
            } else {
                truncate_for_display(&label, label_w)
            };

            let label_style = if is_dirty {
                Style::default()
                    .fg(Color::LightYellow)
                    .add_modifier(Modifier::BOLD)
            } else if is_current {
                Style::default()
                    .fg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let line = Line::from(vec![
                Span::styled(marker.to_string(), marker_style),
                Span::styled(
                    format!(
                        "{:>width$}",
                        req.method.as_str(),
                        width = (method_w - 1) as usize
                    ),
                    Style::default()
                        .fg(req.method.color())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(label_disp, label_style),
            ]);
            Paragraph::new(line).render(row_area, buf);
        }
    }

    fn render_path_input_cursor(&self, frame: &mut Frame) {
        let Some(input) = &self.path_input else {
            return;
        };
        let area = self.footer_area.get();
        if area.width == 0 || area.height == 0 {
            return;
        }
        let label_width = " Load from:  ".chars().count() as u16;
        let pos = input.cursor.min(input.text.len());
        let char_offset = input.text[..pos].chars().count() as u16;
        let max_x = area.x + area.width.saturating_sub(1);
        let cursor_x = (area.x + label_width).saturating_add(char_offset).min(max_x);
        frame.set_cursor_position((cursor_x, area.y));
    }

    fn render_rename_cursor(&self, frame: &mut Frame) {
        let Some(rename) = &self.renaming else { return; };
        let area = self.rename_input_area.get();
        if area.width < 2 || area.height == 0 {
            return;
        }
        let pos = rename.cursor.min(rename.text.len());
        let char_offset = rename.text[..pos].chars().count() as u16;
        let text_start = area.x + 1;
        let max_x = area.x + area.width.saturating_sub(1);
        let cursor_x = text_start.saturating_add(char_offset).min(max_x);
        frame.set_cursor_position((cursor_x, area.y));
    }

    fn render_body(&self, area: Rect, buf: &mut Buffer) {
        Block::new()
            .borders(Borders::LEFT | Borders::TOP | Borders::RIGHT | Borders::BOTTOM)
            .render(area, buf);

        if area.width < 4 || area.height < 3 {
            return;
        }

        let url_row_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 3,
        };
        self.render_url_row(url_row_area, buf);

        buf[(area.x, area.y + 2)].set_symbol("├");
        buf[(area.x + area.width - 1, area.y + 2)].set_symbol("┤");

        if area.height <= 4 {
            return;
        }

        let below = Rect {
            x: area.x,
            y: area.y + 3,
            width: area.width,
            height: area.height - 4,
        };

        if below.height < 2 {
            return;
        }

        let chunks = Layout::vertical([
            Constraint::Percentage(self.split_ratio),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(below);

        self.render_params(inset_horizontal(chunks[0], 1), buf);
        self.render_section_divider(chunks[1], buf);
        self.render_response_body(inset_horizontal(chunks[2], 1), buf);
    }

    fn render_params(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
        self.render_tab_strip(chunks[0], buf);
        self.render_active_tab_editor(chunks[1], buf);
    }

    fn render_tab_strip(&self, area: Rect, buf: &mut Buffer) {
        let tabs_focused =
            self.focus == Focus::Params && self.params_sub_focus == ParamsSubFocus::Tabs;
        let mut spans: Vec<Span> = Vec::new();
        for (i, tab) in RequestTab::ALL.iter().enumerate() {
            let is_active = *tab == self.active_tab;
            let style = if is_active {
                let mut s = Style::default()
                    .fg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD);
                if tabs_focused {
                    s = s.bg(Color::DarkGray);
                }
                s
            } else if tabs_focused {
                Style::default().fg(Color::Gray)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            spans.push(Span::styled(format!(" {} ", tab.label()), style));
            if i + 1 < RequestTab::ALL.len() {
                spans.push(Span::raw(" "));
            }
        }
        Paragraph::new(Line::from(spans)).render(area, buf);
    }

    fn render_active_tab_editor(&self, area: Rect, buf: &mut Buffer) {
        self.editor_area.set(area);
        if area.width == 0 || area.height == 0 {
            return;
        }
        let editor_focused = self.focus == Focus::Params
            && self.params_sub_focus == ParamsSubFocus::Editor;

        match self.active_tab {
            RequestTab::Params => {
                self.render_kv_editor(&self.params_kv, area, buf, editor_focused);
            }
            RequestTab::Headers => {
                self.render_kv_editor(&self.headers_kv, area, buf, editor_focused);
            }
            RequestTab::Body => {
                self.render_text_editor(&self.body_buf, area, buf, editor_focused);
            }
        }
    }

    fn render_kv_editor(
        &self,
        ed: &KvEditor,
        area: Rect,
        buf: &mut Buffer,
        editor_focused: bool,
    ) {
        if area.width < 14 || area.height < 3 {
            return;
        }

        let cb_w: u16 = 5;
        let remaining = area.width - cb_w;
        let key_w = remaining / 2;
        let value_x = area.x + cb_w + key_w;
        let value_w = remaining - key_w;
        let key_x = area.x + cb_w;

        let header_style = Style::default().fg(Color::DarkGray);
        Paragraph::new(Line::from(Span::styled(" Key", header_style))).render(
            Rect { x: key_x, y: area.y, width: key_w, height: 1 },
            buf,
        );
        Paragraph::new(Line::from(Span::styled(" Value", header_style))).render(
            Rect { x: value_x, y: area.y, width: value_w, height: 1 },
            buf,
        );

        for x in area.x..area.x + area.width {
            let cell = &mut buf[(x, area.y + 1)];
            cell.set_symbol("─");
            cell.set_style(header_style);
        }

        let rows_y = area.y + 2;
        let max_rows = area.height.saturating_sub(2) as usize;

        for (display_i, i) in (ed.scroll_y..(ed.scroll_y + max_rows).min(ed.rows.len())).enumerate()
        {
            let row = &ed.rows[i];
            let y = rows_y + display_i as u16;
            let is_active_row = i == ed.cur_row && editor_focused;
            let disabled = !row.enabled;

            let cb_active = is_active_row && ed.cur_col == KvColumn::Enabled;
            let cb_cell = Rect { x: area.x, y, width: cb_w, height: 1 };
            if cb_active {
                Block::new().bg(Color::DarkGray).render(cb_cell, buf);
            }
            let (cb_text, cb_color) = if row.enabled {
                ("[x]", Color::Green)
            } else {
                ("[ ]", Color::DarkGray)
            };
            Paragraph::new(cb_text)
                .style(Style::default().fg(cb_color).add_modifier(Modifier::BOLD))
                .centered()
                .render(cb_cell, buf);

            let key_active = is_active_row && ed.cur_col == KvColumn::Key;
            let key_cell = Rect { x: key_x, y, width: key_w, height: 1 };
            if key_active {
                Block::new().bg(Color::DarkGray).render(key_cell, buf);
            }
            Paragraph::new(cell_line(&row.key, "(key)", disabled, key_active)).render(key_cell, buf);

            let value_active = is_active_row && ed.cur_col == KvColumn::Value;
            let value_cell = Rect { x: value_x, y, width: value_w, height: 1 };
            if value_active {
                Block::new().bg(Color::DarkGray).render(value_cell, buf);
            }
            Paragraph::new(cell_line(&row.value, "(value)", disabled, value_active)).render(value_cell, buf);
        }
    }

    fn render_text_editor(
        &self,
        text_buf: &TextBuffer,
        area: Rect,
        buf: &mut Buffer,
        editor_focused: bool,
    ) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        if text_buf.is_empty() && !editor_focused {
            let style = Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC);
            Paragraph::new(self.active_tab.placeholder())
                .style(style)
                .render(area, buf);
            return;
        }
        let lines: Vec<Line> = text_buf
            .lines
            .iter()
            .skip(text_buf.scroll_y)
            .take(area.height as usize)
            .map(|s| Line::from(s.as_str()))
            .collect();
        Paragraph::new(lines).render(area, buf);
    }

    fn render_editor_cursor(&self, frame: &mut Frame) {
        let area = self.editor_area.get();
        if area.width == 0 || area.height == 0 {
            return;
        }
        match self.active_tab {
            RequestTab::Params => self.render_kv_cursor(&self.params_kv, area, frame),
            RequestTab::Headers => self.render_kv_cursor(&self.headers_kv, area, frame),
            RequestTab::Body => self.render_text_cursor(&self.body_buf, area, frame),
        }
    }

    fn render_kv_cursor(&self, ed: &KvEditor, area: Rect, frame: &mut Frame) {
        if area.width < 14 || area.height < 3 {
            return;
        }
        if !ed.is_text_cell() {
            return;
        }

        let cb_w: u16 = 5;
        let remaining = area.width - cb_w;
        let key_w = remaining / 2;
        let value_x = area.x + cb_w + key_w;
        let value_w = remaining - key_w;
        let key_x = area.x + cb_w;

        let visual_row = ed.cur_row.saturating_sub(ed.scroll_y);
        let cursor_y = area.y + 2 + visual_row as u16;
        if cursor_y >= area.y + area.height {
            return;
        }

        let cell_text = match ed.cur_col {
            KvColumn::Key => &ed.rows[ed.cur_row].key,
            KvColumn::Value => &ed.rows[ed.cur_row].value,
            KvColumn::Enabled => return,
        };
        let pos = ed.cur_pos.min(cell_text.len());
        let char_col = cell_text[..pos].chars().count() as u16;

        let (cell_x, cell_width) = match ed.cur_col {
            KvColumn::Key => (key_x, key_w),
            KvColumn::Value => (value_x, value_w),
            KvColumn::Enabled => return,
        };

        let text_start = cell_x + 1;
        let max_x = cell_x + cell_width.saturating_sub(1);
        let cursor_x = text_start.saturating_add(char_col).min(max_x);

        frame.set_cursor_position((cursor_x, cursor_y));
    }

    fn render_text_cursor(&self, text_buf: &TextBuffer, area: Rect, frame: &mut Frame) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let visual_row = text_buf.cursor_row.saturating_sub(text_buf.scroll_y);
        if visual_row >= area.height as usize {
            return;
        }
        let line = &text_buf.lines[text_buf.cursor_row];
        let col_bytes = text_buf.cursor_col.min(line.len());
        let char_col = line[..col_bytes].chars().count();
        if char_col >= area.width as usize {
            return;
        }
        frame.set_cursor_position((area.x + char_col as u16, area.y + visual_row as u16));
    }

    fn render_section_divider(&self, area: Rect, buf: &mut Buffer) {
        if area.width < 4 {
            return;
        }
        let (title, title_color) = match &self.response {
            ResponseState::Empty => (" RESPONSE ".to_string(), Color::LightCyan),
            ResponseState::InFlight => (" RESPONSE  sending… ".to_string(), Color::Yellow),
            ResponseState::Done(d) => (
                format!(" RESPONSE  {} {} · {}ms ", d.status, d.status_text, d.elapsed_ms),
                status_color(d.status),
            ),
            ResponseState::Error(_) => (" RESPONSE  ERROR ".to_string(), Color::Red),
        };

        let show_icon = matches!(
            self.response,
            ResponseState::Done(_) | ResponseState::Error(_)
        );
        let icon = "[⧉]";
        let icon_width = if show_icon { icon.chars().count() } else { 0 };

        let total = area.width as usize;
        let left_dashes = 1usize;
        let title_room = total.saturating_sub(2 + left_dashes + icon_width);
        let title_trunc: String = title.chars().take(title_room).collect();
        let title_len = title_trunc.chars().count();
        let right_dashes = total - 2 - left_dashes - title_len - icon_width;

        let mut title_style = Style::default().fg(title_color).add_modifier(Modifier::BOLD);
        if self.focus == Focus::Response {
            title_style = title_style.bg(Color::DarkGray);
        }

        let mut spans = vec![
            Span::raw("├"),
            Span::raw("─".repeat(left_dashes)),
            Span::styled(title_trunc, title_style),
            Span::raw("─".repeat(right_dashes)),
        ];
        if show_icon {
            let icon_style = if self.focus == Focus::Response {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD)
            };
            spans.push(Span::styled(icon, icon_style));
        }
        spans.push(Span::raw("┤"));

        Paragraph::new(Line::from(spans)).render(area, buf);
    }

    fn render_response_body(&self, area: Rect, buf: &mut Buffer) {
        self.response_area.set(area);
        if area.width == 0 || area.height == 0 {
            self.response_max_scroll.set(0);
            return;
        }
        let body = match &self.response {
            ResponseState::Empty => String::new(),
            ResponseState::InFlight => String::new(),
            ResponseState::Done(d) => d.body.clone(),
            ResponseState::Error(e) => e.clone(),
        };

        let total = wrapped_line_count(&body, area.width);
        let max_scroll = total.saturating_sub(area.height);
        self.response_max_scroll.set(max_scroll);
        let scroll = self.response_scroll.min(max_scroll);
        Paragraph::new(body)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .render(area, buf);
    }

    fn render_url_row(&self, area: Rect, buf: &mut Buffer) {
        self.url_row_area.set(area);

        let outer = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Gray));
        let inner = outer.inner(area);
        outer.render(area, buf);

        if inner.width < 14 {
            return;
        }

        let chunks = Layout::horizontal([
            Constraint::Length(9),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(8),
        ])
        .split(inner);

        self.render_method_cell(chunks[0], buf);
        Self::render_separator(chunks[1], area, buf);
        self.render_url_cell(chunks[2], buf);
        Self::render_separator(chunks[3], area, buf);
        self.render_send_cell(chunks[4], buf);
    }

    fn render_separator(sep_area: Rect, row_area: Rect, buf: &mut Buffer) {
        for y in sep_area.y..sep_area.y + sep_area.height {
            buf[(sep_area.x, y)].set_symbol("│");
        }
        let top_y = row_area.y;
        let bot_y = row_area.y + row_area.height - 1;
        buf[(sep_area.x, top_y)].set_symbol("┬");
        buf[(sep_area.x, bot_y)].set_symbol("┴");
    }

    fn render_method_cell(&self, area: Rect, buf: &mut Buffer) {
        self.method_area.set(area);
        let focused = self.focus == Focus::Method;

        if focused {
            Block::new().bg(Color::DarkGray).render(area, buf);
        }

        let style = Style::default()
            .fg(self.method.color())
            .add_modifier(Modifier::BOLD);
        Paragraph::new(self.method.as_str())
            .style(style)
            .centered()
            .render(area, buf);
    }

    fn render_url_cell(&self, area: Rect, buf: &mut Buffer) {
        self.url_area.set(area);
        let focused = self.focus == Focus::Url;

        if focused {
            Block::new().bg(Color::DarkGray).render(area, buf);
        }

        if area.width < 2 {
            return;
        }
        let text_area = Rect {
            x: area.x + 1,
            y: area.y,
            width: area.width - 2,
            height: area.height,
        };
        Paragraph::new(self.url.as_str()).render(text_area, buf);
    }

    fn render_send_cell(&self, area: Rect, buf: &mut Buffer) {
        self.send_area.set(area);
        let focused = self.focus == Focus::Send;
        let in_flight = matches!(self.response, ResponseState::InFlight);

        if focused {
            Block::new().bg(Color::DarkGray).render(area, buf);
        }

        let (label, fg) = if in_flight {
            ("…", Color::Yellow)
        } else {
            ("Send", Color::LightGreen)
        };

        let style = Style::default().fg(fg).add_modifier(Modifier::BOLD);
        Paragraph::new(label)
            .style(style)
            .centered()
            .render(area, buf);
    }

    fn render_url_cursor(&self, frame: &mut Frame) {
        let area = self.url_area.get();
        if area.width < 2 || area.height == 0 {
            return;
        }
        let char_offset = self.url[..self.url_cursor].chars().count() as u16;
        let text_x = area.x + 1;
        let max_x = area.x + area.width - 2;
        let cursor_x = text_x.saturating_add(char_offset).min(max_x);
        frame.set_cursor_position((cursor_x, area.y));
    }

    fn render_method_dropdown(&self, frame: &mut Frame) {
        let row_area = self.url_row_area.get();
        let method_area = self.method_area.get();
        if method_area.width == 0 || row_area.width == 0 {
            return;
        }

        let methods = HttpMethod::ALL;
        let dropdown_height = methods.len() as u16 + 2;
        let desired = Rect {
            x: method_area.x.saturating_sub(1),
            y: row_area.y + row_area.height,
            width: 14,
            height: dropdown_height,
        };
        let dropdown_area = desired.intersection(frame.area());
        if dropdown_area.width == 0 || dropdown_area.height == 0 {
            return;
        }

        frame.render_widget(Clear, dropdown_area);

        let lines: Vec<Line> = methods
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let mut style = Style::default()
                    .fg(m.color())
                    .add_modifier(Modifier::BOLD);
                if i == self.method_dropdown_index {
                    style = style.bg(Color::DarkGray);
                }
                Line::from(format!(" {}", m.as_str())).style(style)
            })
            .collect();

        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::LightCyan));
        frame.render_widget(Paragraph::new(lines).block(block), dropdown_area);
    }
}

fn cell_line<'a>(text: &'a str, placeholder: &'a str, disabled: bool, active: bool) -> Line<'a> {
    if text.is_empty() && !active {
        return Line::from(Span::styled(
            format!(" {}", placeholder),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ));
    }
    let style = if disabled {
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::CROSSED_OUT)
    } else {
        Style::default()
    };
    Line::from(Span::styled(format!(" {}", text), style))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedRequest {
    #[serde(default)]
    name: Option<String>,
    method: HttpMethod,
    url: String,
    params: Vec<KvRow>,
    headers: Vec<KvRow>,
    body: String,
    last_response: Option<ResponseDisplay>,
}

fn collection_file_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home).join(".postui_collection.json")
}

fn load_collection_from_disk() -> Vec<SavedRequest> {
    let path = collection_file_path();
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn copy_to_clipboard(text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::Stdio;

    #[cfg(target_os = "macos")]
    let candidates: &[(&str, &[&str])] = &[("pbcopy", &[])];

    #[cfg(target_os = "linux")]
    let candidates: &[(&str, &[&str])] = &[
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["-b", "-i"]),
    ];

    #[cfg(target_os = "windows")]
    let candidates: &[(&str, &[&str])] = &[("clip", &[])];

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let candidates: &[(&str, &[&str])] = &[];

    let mut last_err = String::new();
    for (cmd, args) in candidates {
        let spawned = std::process::Command::new(cmd)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Some(stdin) = child.stdin.as_mut() {
            if let Err(e) = stdin.write_all(text.as_bytes()) {
                last_err = format!("{cmd}: {e}");
                continue;
            }
        }
        drop(child.stdin.take());
        match child.wait() {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => last_err = format!("{cmd} exited {status}"),
            Err(e) => last_err = format!("{cmd}: {e}"),
        }
    }
    if last_err.is_empty() {
        Err("no clipboard command found".into())
    } else {
        Err(last_err)
    }
}

fn kv_rows_equivalent(a: &[KvRow], b: &[KvRow]) -> bool {
    let trim = |rows: &[KvRow]| -> Vec<KvRow> {
        let mut v: Vec<KvRow> = rows.to_vec();
        while let Some(last) = v.last() {
            if last.key.is_empty() && last.value.is_empty() {
                v.pop();
            } else {
                break;
            }
        }
        v
    };
    trim(a) == trim(b)
}

fn expand_tilde(s: &str) -> String {
    if s == "~" {
        return std::env::var("HOME").unwrap_or_else(|_| ".".into());
    }
    if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{}/{}", home, rest);
        }
    }
    s.to_string()
}

fn truncate_for_display(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn wrapped_line_count(text: &str, width: u16) -> u16 {
    if width == 0 {
        return 0;
    }
    let w = width as usize;
    let mut total: u32 = 0;
    for line in text.split('\n') {
        if line.is_empty() {
            total += 1;
        } else {
            let len = line.chars().count();
            total += ((len + w - 1) / w) as u32;
        }
    }
    total.min(u16::MAX as u32) as u16
}

fn inset_horizontal(area: Rect, inset: u16) -> Rect {
    if area.width < inset * 2 {
        return Rect { x: area.x, y: area.y, width: 0, height: area.height };
    }
    Rect {
        x: area.x + inset,
        y: area.y,
        width: area.width - inset * 2,
        height: area.height,
    }
}

fn status_color(status: u16) -> Color {
    match status {
        200..=299 => Color::Green,
        300..=399 => Color::Yellow,
        400..=499 => Color::LightRed,
        500..=599 => Color::Red,
        _ => Color::Gray,
    }
}

impl Widget for &App {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut rows = Vec::new();
        if self.show_header {
            rows.push(Constraint::Length(1));
        }
        rows.push(Constraint::Min(0));
        if self.show_footer {
            rows.push(Constraint::Length(1));
        }

        let vchunks = Layout::vertical(rows).split(area);

        let mut i = 0;

        if self.show_header {
            Paragraph::new(Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    self.name.as_str(),
                    Style::default()
                        .fg(Color::LightCyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ]))
            .render(vchunks[i], buf);
            i += 1;
        }

        let body = vchunks[i];
        i += 1;

        if self.show_sidebar {
            let hchunks = Layout::horizontal([
                Constraint::Length(30),
                Constraint::Min(0),
            ])
            .split(body);

            self.render_sidebar(hchunks[0], buf);
            self.render_body(hchunks[1], buf);
        } else {
            self.render_body(body, buf);
        }

        if self.show_footer {
            self.footer_area.set(vchunks[i]);
            if let Some(input) = &self.path_input {
                let label_style = Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightYellow)
                    .add_modifier(Modifier::BOLD);
                let line = Line::from(vec![
                    Span::styled(" Load from: ", label_style),
                    Span::raw(" "),
                    Span::styled(
                        input.text.as_str(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ]);
                Paragraph::new(line).render(vchunks[i], buf);
                return;
            }
            let active_status = self.status_message.as_ref().and_then(|(msg, when)| {
                if when.elapsed() < Duration::from_secs(3) {
                    Some(msg.as_str())
                } else {
                    None
                }
            });
            if let Some(msg) = active_status {
                Paragraph::new(format!(" {}", msg))
                    .style(
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::LightGreen)
                            .add_modifier(Modifier::BOLD),
                    )
                    .render(vchunks[i], buf);
            } else {
                let hint: &str = if self.method_dropdown_open {
                    " ↑/↓: navigate  enter: select  esc: cancel "
                } else {
                    match self.focus {
                        Focus::Method => " q/^C: quit  ^B: sidebar  ^S: save  ^O: load  tab: focus  enter: open method ",
                        Focus::Url => " ^C: quit  ^B: sidebar  ^S: save  tab: focus  enter: send  type to edit ",
                        Focus::Send => " q/^C: quit  ^B: sidebar  ^S: save  tab: focus  enter/space: send ",
                        Focus::Params => match self.params_sub_focus {
                            ParamsSubFocus::Tabs => " tab/→: next tab  shift+tab/←: prev tab  ↓/enter: edit  ^S: save  ^O: load ",
                            ParamsSubFocus::Editor => match self.active_tab {
                                RequestTab::Body => " esc/↑: tabs  shift+tab: prev tab  tab: indent  ^S: save  ^O: load ",
                                _ => " tab/⇧tab: next/prev cell  esc/↑: tabs  enter: next/toggle  space: toggle  ^D: del row ",
                            },
                        },
                        Focus::Response => " ↑/↓: scroll  PgUp/PgDn: page  c: copy  Home/End: top/bot  ⇧↑/⇧↓: resize ",
                        Focus::Sidebar => {
                            if self.renaming.is_some() {
                                " enter: save  esc: cancel  type to edit "
                            } else {
                                " ↑/↓: select  enter: load  r: rename  d: delete  ^B: leave  ^S: save  ^O: reload "
                            }
                        }
                    }
                };
                Paragraph::new(hint).render(vchunks[i], buf);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(enabled: bool, key: &str, value: &str) -> KvRow {
        KvRow {
            enabled,
            key: key.into(),
            value: value.into(),
        }
    }

    // ----- TextBuffer -----

    #[test]
    fn text_buffer_default_has_one_empty_line() {
        let b = TextBuffer::new();
        assert_eq!(b.lines, vec![String::new()]);
        assert_eq!(b.cursor_row, 0);
        assert_eq!(b.cursor_col, 0);
        assert_eq!(b.text(), "");
        assert!(b.is_empty());
    }

    #[test]
    fn text_buffer_from_text_roundtrip() {
        for s in ["", "abc", "a\nb", "a\n\nb", "α\nβ", "  spaces  "] {
            let b = TextBuffer::from_text(s);
            assert_eq!(b.text(), s, "input: {s:?}");
        }
    }

    #[test]
    fn text_buffer_insert_char_appends() {
        let mut b = TextBuffer::new();
        b.insert_char('a');
        b.insert_char('b');
        b.insert_char('c');
        assert_eq!(b.text(), "abc");
        assert_eq!(b.cursor_col, 3);
    }

    #[test]
    fn text_buffer_insert_newline_splits() {
        let mut b = TextBuffer::from_text("abc");
        b.cursor_col = 1;
        b.insert_newline();
        assert_eq!(b.text(), "a\nbc");
        assert_eq!(b.cursor_row, 1);
        assert_eq!(b.cursor_col, 0);
    }

    #[test]
    fn text_buffer_backspace_within_line() {
        let mut b = TextBuffer::from_text("abc");
        b.move_end();
        b.backspace();
        assert_eq!(b.text(), "ab");
    }

    #[test]
    fn text_buffer_backspace_at_line_start_merges_with_prev() {
        let mut b = TextBuffer::from_text("a\nb");
        b.cursor_row = 1;
        b.cursor_col = 0;
        b.backspace();
        assert_eq!(b.text(), "ab");
        assert_eq!(b.cursor_row, 0);
        assert_eq!(b.cursor_col, 1);
    }

    #[test]
    fn text_buffer_delete_within_line() {
        let mut b = TextBuffer::from_text("abc");
        b.cursor_col = 0;
        b.delete();
        assert_eq!(b.text(), "bc");
    }

    #[test]
    fn text_buffer_delete_at_line_end_merges_with_next() {
        let mut b = TextBuffer::from_text("a\nb");
        b.cursor_col = 1;
        b.delete();
        assert_eq!(b.text(), "ab");
    }

    #[test]
    fn text_buffer_move_left_at_line_start_jumps_prev_line_end() {
        let mut b = TextBuffer::from_text("a\nb");
        b.cursor_row = 1;
        b.cursor_col = 0;
        b.move_left();
        assert_eq!(b.cursor_row, 0);
        assert_eq!(b.cursor_col, 1);
    }

    #[test]
    fn text_buffer_move_right_at_line_end_jumps_next_line_start() {
        let mut b = TextBuffer::from_text("a\nb");
        b.cursor_row = 0;
        b.cursor_col = 1;
        b.move_right();
        assert_eq!(b.cursor_row, 1);
        assert_eq!(b.cursor_col, 0);
    }

    #[test]
    fn text_buffer_move_down_clamps_to_shorter_line() {
        let mut b = TextBuffer::from_text("longer\nx");
        b.cursor_row = 0;
        b.cursor_col = 5;
        b.move_down();
        assert_eq!(b.cursor_row, 1);
        assert_eq!(b.cursor_col, 1);
    }

    #[test]
    fn text_buffer_utf8_chars_use_byte_offsets() {
        let mut b = TextBuffer::new();
        b.insert_char('α');
        b.insert_char('β');
        assert_eq!(b.text(), "αβ");
        assert_eq!(b.cursor_col, 4);
        b.move_left();
        assert_eq!(b.cursor_col, 2);
    }

    #[test]
    fn text_buffer_ensure_visible_scrolls_when_cursor_below() {
        let mut b = TextBuffer::from_text("a\nb\nc\nd\ne");
        b.cursor_row = 4;
        b.ensure_visible(2);
        assert_eq!(b.scroll_y, 3);
    }

    #[test]
    fn text_buffer_ensure_visible_scrolls_up_when_cursor_above() {
        let mut b = TextBuffer::from_text("a\nb\nc\nd\ne");
        b.scroll_y = 3;
        b.cursor_row = 1;
        b.ensure_visible(2);
        assert_eq!(b.scroll_y, 1);
    }

    // ----- KvEditor -----

    #[test]
    fn kv_editor_default_state() {
        let e = KvEditor::new();
        assert_eq!(e.rows.len(), 1);
        assert_eq!(e.cur_row, 0);
        assert_eq!(e.cur_col, KvColumn::Key);
        assert_eq!(e.cur_pos, 0);
        assert!(e.rows[0].enabled);
        assert!(e.rows[0].key.is_empty());
        assert!(e.rows[0].value.is_empty());
    }

    #[test]
    fn kv_editor_from_rows_empty_synthesizes_placeholder() {
        let e = KvEditor::from_rows(vec![]);
        assert_eq!(e.rows.len(), 1);
        assert_eq!(e.rows[0], KvRow::new());
    }

    #[test]
    fn kv_editor_from_rows_preserves_given() {
        let rows = vec![kv(true, "k", "v"), kv(false, "x", "y")];
        let e = KvEditor::from_rows(rows.clone());
        assert_eq!(e.rows, rows);
    }

    #[test]
    fn kv_editor_insert_char_only_on_text_cells() {
        let mut e = KvEditor::new();
        e.insert_char('k');
        assert_eq!(e.rows[0].key, "k");
        e.cur_col = KvColumn::Enabled;
        e.cur_pos = 0;
        e.insert_char('x');
        assert_eq!(e.rows[0].key, "k");
    }

    #[test]
    fn kv_editor_advance_cell_walks_key_to_value_to_next_row() {
        let mut e = KvEditor::new();
        assert_eq!(e.cur_col, KvColumn::Key);
        e.advance_cell();
        assert_eq!(e.cur_col, KvColumn::Value);
        assert_eq!(e.cur_row, 0);
        e.advance_cell();
        assert_eq!(e.rows.len(), 2);
        assert_eq!(e.cur_row, 1);
        assert_eq!(e.cur_col, KvColumn::Key);
    }

    #[test]
    fn kv_editor_retreat_cell_walks_value_to_key_to_prev_row() {
        let mut e = KvEditor::from_rows(vec![kv(true, "a", "1"), kv(true, "b", "2")]);
        e.cur_row = 1;
        e.cur_col = KvColumn::Value;
        e.cur_pos = e.rows[1].value.len();
        e.retreat_cell();
        assert_eq!(e.cur_row, 1);
        assert_eq!(e.cur_col, KvColumn::Key);
        e.retreat_cell();
        assert_eq!(e.cur_row, 0);
        assert_eq!(e.cur_col, KvColumn::Value);
    }

    #[test]
    fn kv_editor_toggle_enabled_flips() {
        let mut e = KvEditor::new();
        assert!(e.rows[0].enabled);
        e.toggle_enabled();
        assert!(!e.rows[0].enabled);
        e.toggle_enabled();
        assert!(e.rows[0].enabled);
    }

    #[test]
    fn kv_editor_delete_current_row_keeps_at_least_one() {
        let mut e = KvEditor::new();
        e.insert_char('k');
        e.delete_current_row();
        assert_eq!(e.rows.len(), 1);
        assert_eq!(e.rows[0], KvRow::new());
        assert_eq!(e.cur_row, 0);
    }

    #[test]
    fn kv_editor_delete_current_row_removes_when_multiple() {
        let mut e = KvEditor::from_rows(vec![kv(true, "a", "1"), kv(true, "b", "2")]);
        e.cur_row = 0;
        e.delete_current_row();
        assert_eq!(e.rows.len(), 1);
        assert_eq!(e.rows[0].key, "b");
    }

    #[test]
    fn kv_editor_entries_filters_disabled_and_empty_keys_and_trims() {
        let e = KvEditor::from_rows(vec![
            kv(true, "  a  ", "  1  "),
            kv(false, "skip", "x"),
            kv(true, "", "no-key"),
            kv(true, "b", ""),
        ]);
        assert_eq!(
            e.entries(),
            vec![("a".into(), "1".into()), ("b".into(), "".into())]
        );
    }

    #[test]
    fn kv_editor_move_left_at_value_start_jumps_to_key_end() {
        let mut e = KvEditor::from_rows(vec![kv(true, "key", "val")]);
        e.cur_col = KvColumn::Value;
        e.cur_pos = 0;
        e.move_left();
        assert_eq!(e.cur_col, KvColumn::Key);
        assert_eq!(e.cur_pos, 3);
    }

    #[test]
    fn kv_editor_move_right_at_key_end_jumps_to_value_start() {
        let mut e = KvEditor::from_rows(vec![kv(true, "key", "val")]);
        e.cur_col = KvColumn::Key;
        e.cur_pos = 3;
        e.move_right();
        assert_eq!(e.cur_col, KvColumn::Value);
        assert_eq!(e.cur_pos, 0);
    }

    // ----- Dirty comparison helper -----

    #[test]
    fn kv_rows_equivalent_strips_trailing_empties() {
        let saved = vec![kv(true, "k", "v")];
        let live = vec![kv(true, "k", "v"), KvRow::new()];
        assert!(kv_rows_equivalent(&saved, &live));
    }

    #[test]
    fn kv_rows_equivalent_detects_difference() {
        let saved = vec![kv(true, "k", "v")];
        let live = vec![kv(true, "k", "v2")];
        assert!(!kv_rows_equivalent(&saved, &live));
    }

    #[test]
    fn kv_rows_equivalent_treats_both_empty_as_equal() {
        let saved: Vec<KvRow> = vec![];
        let live = vec![KvRow::new()];
        assert!(kv_rows_equivalent(&saved, &live));
    }

    #[test]
    fn kv_rows_equivalent_distinguishes_enabled_flag() {
        let saved = vec![kv(true, "k", "v")];
        let live = vec![kv(false, "k", "v")];
        assert!(!kv_rows_equivalent(&saved, &live));
    }

    // ----- Free function helpers -----

    #[test]
    fn expand_tilde_replaces_prefix_with_home() {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("~/foo/bar"), format!("{home}/foo/bar"));
        assert_eq!(expand_tilde("/abs/path"), "/abs/path");
        assert_eq!(expand_tilde("relative"), "relative");
        assert_eq!(expand_tilde(""), "");
    }

    #[test]
    fn truncate_for_display_short_returns_unchanged() {
        assert_eq!(truncate_for_display("abc", 10), "abc");
        assert_eq!(truncate_for_display("", 5), "");
    }

    #[test]
    fn truncate_for_display_long_adds_ellipsis() {
        let out = truncate_for_display("abcdefghij", 5);
        assert_eq!(out.chars().count(), 5);
        assert!(out.ends_with('…'));
        assert!(out.starts_with("abcd"));
    }

    #[test]
    fn wrapped_line_count_handles_basic_cases() {
        assert_eq!(wrapped_line_count("", 10), 1);
        assert_eq!(wrapped_line_count("abc", 10), 1);
        assert_eq!(wrapped_line_count("a\nb", 10), 2);
        assert_eq!(wrapped_line_count("abcdef", 3), 2);
        assert_eq!(wrapped_line_count("anything", 0), 0);
    }

    #[test]
    fn inset_horizontal_shrinks_by_2x_inset() {
        let r = Rect { x: 5, y: 0, width: 10, height: 1 };
        let inner = inset_horizontal(r, 2);
        assert_eq!(inner.x, 7);
        assert_eq!(inner.width, 6);
        assert_eq!(inner.y, 0);
        assert_eq!(inner.height, 1);
    }

    #[test]
    fn inset_horizontal_returns_zero_width_when_too_narrow() {
        let r = Rect { x: 5, y: 0, width: 3, height: 1 };
        let inner = inset_horizontal(r, 2);
        assert_eq!(inner.width, 0);
    }

    #[test]
    fn status_color_buckets_by_class() {
        assert_eq!(status_color(200), Color::Green);
        assert_eq!(status_color(204), Color::Green);
        assert_eq!(status_color(301), Color::Yellow);
        assert_eq!(status_color(404), Color::LightRed);
        assert_eq!(status_color(500), Color::Red);
        assert_eq!(status_color(999), Color::Gray);
    }

    // ----- HttpMethod / RequestTab / SidebarCursor -----

    #[test]
    fn http_method_allows_body_only_for_mutating_verbs() {
        assert!(!HttpMethod::Get.allows_body());
        assert!(!HttpMethod::Head.allows_body());
        assert!(!HttpMethod::Options.allows_body());
        assert!(HttpMethod::Post.allows_body());
        assert!(HttpMethod::Put.allows_body());
        assert!(HttpMethod::Patch.allows_body());
        assert!(HttpMethod::Delete.allows_body());
    }

    #[test]
    fn http_method_as_str() {
        assert_eq!(HttpMethod::Get.as_str(), "GET");
        assert_eq!(HttpMethod::Post.as_str(), "POST");
        assert_eq!(HttpMethod::Options.as_str(), "OPTIONS");
    }

    #[test]
    fn sidebar_cursor_saved_index() {
        assert_eq!(SidebarCursor::NewRequest.saved_index(), None);
        assert_eq!(SidebarCursor::Saved(0).saved_index(), Some(0));
        assert_eq!(SidebarCursor::Saved(7).saved_index(), Some(7));
    }

    // ----- Serde roundtrip -----

    #[test]
    fn saved_request_roundtrip() {
        let req = SavedRequest {
            name: Some("greet".into()),
            method: HttpMethod::Post,
            url: "https://example.com/x".into(),
            params: vec![kv(true, "p", "1"), kv(false, "q", "2")],
            headers: vec![kv(true, "Accept", "application/json")],
            body: "{\"hello\":\"world\"}".into(),
            last_response: Some(ResponseDisplay {
                status: 200,
                status_text: "OK".into(),
                body: "{}".into(),
                elapsed_ms: 42,
            }),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: SavedRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, req.name);
        assert_eq!(back.method, req.method);
        assert_eq!(back.url, req.url);
        assert_eq!(back.params, req.params);
        assert_eq!(back.headers, req.headers);
        assert_eq!(back.body, req.body);
        let r = back.last_response.unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.elapsed_ms, 42);
    }

    #[test]
    fn saved_request_name_defaults_when_absent() {
        let json = r#"{
            "method": "Get",
            "url": "",
            "params": [],
            "headers": [],
            "body": "",
            "last_response": null
        }"#;
        let req: SavedRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.name, None);
        assert_eq!(req.method, HttpMethod::Get);
    }
}
