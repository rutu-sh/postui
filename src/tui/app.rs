use std::io;

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind}
};
use ratatui::{
    DefaultTerminal, 
    Frame, 
    buffer::Buffer, 
    layout::{Constraint, Layout, Rect}, 
    style::{Color,Stylize}, 
    text::Line, 
    widgets::{Block, BorderType, Borders, Paragraph, Widget}
};

#[derive(Debug)]
pub struct App {
    name: String,
    exit: bool,
    show_header: bool,
    show_sidebar: bool,
    show_footer: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            name: "postui".into(),
            exit: false,
            show_header: true,
            show_sidebar: true,
            show_footer: true,
        }
    }
}

impl App {
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        while !self.exit {
            terminal.draw(|frame| self.draw(frame))?;
            self.handle_events()?;
        }
        Ok(())
    }

    fn draw(&self, frame: &mut Frame) {
        frame.render_widget(self, frame.area());
    }

    fn handle_events(&mut self) -> io::Result<()> {
        if let Event::Key(key) = event::read()? && key.kind == KeyEventKind::Press {
                self.handle_key(key);
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.exit(),
            KeyCode::Char('h') => self.show_header = !self.show_header,
            KeyCode::Char('s') => self.show_sidebar = !self.show_sidebar,
            KeyCode::Char('f') => self.show_footer = !self.show_footer,
            _ => {}
        }
    }

    fn exit(&mut self) {
        self.exit = true;
    }

    fn render_body(&self, area: Rect, buf: &mut Buffer) {
        let body_block = Block::new()
            .borders(Borders::LEFT | Borders::TOP | Borders::RIGHT | Borders::BOTTOM);
        let inner = body_block.inner(area);
        body_block.render(area, buf);

        let rows = vec![Constraint::Percentage(50), Constraint::Min(0)];
        let vchunks = Layout::vertical(rows).split(inner);
        self.render_request_block(vchunks[0], buf);
        self.render_response_block(vchunks[1], buf);
    }

    fn render_request_block(&self, area: Rect, buf: &mut Buffer) {
        /*
        let req_block = Block::bordered()
            .title(" REQUEST ")
            .title_style(Color::LightCyan)
            .border_type(BorderType::Rounded);
        let inner = req_block.inner(area);
        req_block.render(area, buf);
        */

        let rows = vec![Constraint::Length(3), Constraint::Min(0)];
        let vchunks = Layout::vertical(rows).split(area);

        self.render_url_block(vchunks[0], buf);
    }

    fn render_url_block(&self, area: Rect, buf: &mut Buffer) {
        let columns = vec![Constraint::Length(16), Constraint::Min(0)];
        let hchunks = Layout::horizontal(columns).split(area);

        self.render_method(hchunks[0], buf);
        self.render_url(hchunks[1], buf);
    }

    fn render_response_block(&self, area: Rect, buf: &mut Buffer) {
        Paragraph::new("")
            .block(
                Block::bordered()
                .title(" RESPONSE ")
                .title_style(Color::LightCyan)
                .border_type(BorderType::Rounded)
            )
            .render(area, buf);
    }

    fn render_method(&self, area: Rect, buf: &mut Buffer) {
        Paragraph::new("GET")
            .block(
                Block::new()
                .borders(Borders::TOP | Borders::LEFT | Borders::BOTTOM)
                .title("Method")
                .title_style(Color::Red)
                .border_type(BorderType::LightDoubleDashed)
            )
            .centered()
            .render(area, buf);
    }

    fn render_url(&self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(" https://me.rutu-sh.com")
            .block(Block::bordered()
                .title("URL")
                .title_style(Color::Red)
                .border_type(BorderType::LightDoubleDashed))
            .render(area, buf);
    }
}

impl Widget for &App {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut rows = Vec::new();
        if self.show_header {
            rows.push(Constraint::Length(3));
        }
        rows.push(Constraint::Min(0));
        if self.show_footer {
            rows.push(Constraint::Length(1));
        }

        let vchunks = Layout::vertical(rows).split(area);

        let mut i = 0;

        if self.show_header {
            Block::bordered()
                .title(Line::from(format!(" {} ", self.name)))
                .border_type(BorderType::Rounded)
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

            Block::new()
                .borders(Borders::TOP | Borders::LEFT | Borders::BOTTOM)
                .title("Sidebar").title_style(Color::Rgb(0, 255, 0)).bold()
                .render(hchunks[0], buf);
            self.render_body(hchunks[1], buf);
        } else {
            self.render_body(body, buf);
        }

        if self.show_footer {
            Paragraph::new(" q: quit  h: header  s: sidebar  f: footer ")
                .render(vchunks[i], buf);
        }
    }
}
