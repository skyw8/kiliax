use std::io::{self, Stdout, Write};

use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Rect, Size};
use ratatui::Terminal;

use crate::history;

pub struct TerminalGuard;

pub struct TerminalState {
    terminal: Terminal<ViewportBackend<CrosstermBackend<Stdout>>>,
    pending_history_lines: Vec<ratatui::text::Line<'static>>,
    viewport: Rect,
    full_size: Size,
}

impl TerminalState {
    pub fn full_width(&self) -> u16 {
        self.full_size.width
    }

    pub fn queue_history_lines(&mut self, lines: Vec<ratatui::text::Line<'static>>) {
        self.pending_history_lines.extend(lines);
    }

    pub fn draw(&mut self, draw_fn: impl FnOnce(&mut ratatui::Frame)) -> anyhow::Result<()> {
        self.full_size = self.terminal.backend().full_size()?;
        self.viewport = compute_viewport(self.full_size);
        self.terminal.backend_mut().set_viewport(self.viewport);

        if !self.pending_history_lines.is_empty() {
            let lines = std::mem::take(&mut self.pending_history_lines);
            history::insert_history_lines(&lines, self.viewport, self.full_size)?;
        }

        self.terminal.draw(draw_fn)?;
        Ok(())
    }
}

pub fn init() -> anyhow::Result<(TerminalGuard, TerminalState)> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnableBracketedPaste)?;

    let backend = CrosstermBackend::new(io::stdout());
    let viewport_backend = ViewportBackend::new(backend);
    let mut terminal = Terminal::new(viewport_backend)?;
    terminal.clear()?;

    let full_size = terminal.backend().full_size()?;
    let viewport = compute_viewport(full_size);
    terminal.backend_mut().set_viewport(viewport);
    terminal.clear()?;

    Ok((
        TerminalGuard,
        TerminalState {
            terminal,
            pending_history_lines: Vec::new(),
            viewport,
            full_size,
        },
    ))
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableBracketedPaste);
        let _ = execute!(io::stdout(), crossterm::cursor::Show);
        let _ = io::stdout().write_all(b"\x1b[r\x1b[0m");
        let _ = io::stdout().flush();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Viewport {
    origin: Position,
    size: Size,
}

fn compute_viewport(full_size: Size) -> Rect {
    let height = full_size.height;
    if height <= 1 {
        return Rect::new(0, 0, full_size.width, height);
    }

    let max_height = height.saturating_sub(1);
    let min_height = 6u16.min(max_height);
    let desired = 10u16.min(max_height);
    let viewport_height = desired.max(min_height);
    let y = height.saturating_sub(viewport_height);

    Rect::new(0, y, full_size.width, viewport_height)
}

pub struct ViewportBackend<B>
where
    B: Backend + Write,
{
    inner: B,
    viewport: Viewport,
}

impl ViewportBackend<CrosstermBackend<Stdout>> {
    fn new(inner: CrosstermBackend<Stdout>) -> Self {
        Self {
            inner,
            viewport: Viewport {
                origin: Position { x: 0, y: 0 },
                size: Size::new(0, 0),
            },
        }
    }
}

impl<B> ViewportBackend<B>
where
    B: Backend + Write,
{
    pub fn set_viewport(&mut self, rect: Rect) {
        self.viewport = Viewport {
            origin: Position {
                x: rect.x,
                y: rect.y,
            },
            size: Size::new(rect.width, rect.height),
        };
    }

    pub fn full_size(&self) -> io::Result<Size> {
        self.inner.size()
    }

    fn offset(&self, position: Position) -> Position {
        Position {
            x: position.x.saturating_add(self.viewport.origin.x),
            y: position.y.saturating_add(self.viewport.origin.y),
        }
    }

    fn unoffset(&self, position: Position) -> Position {
        Position {
            x: position.x.saturating_sub(self.viewport.origin.x),
            y: position.y.saturating_sub(self.viewport.origin.y),
        }
    }

    fn clear_viewport(&mut self) -> io::Result<()> {
        let mut cells: Vec<(u16, u16, Cell)> = Vec::with_capacity(
            self.viewport.size.width as usize * self.viewport.size.height as usize,
        );
        for y in 0..self.viewport.size.height {
            for x in 0..self.viewport.size.width {
                cells.push((x, y, Cell::default()));
            }
        }
        self.draw(cells.iter().map(|(x, y, cell)| (*x, *y, cell)))
    }
}

impl<B> Backend for ViewportBackend<B>
where
    B: Backend + Write,
{
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let origin = self.viewport.origin;
        self.inner.draw(content.map(move |(x, y, cell)| {
            (x.saturating_add(origin.x), y.saturating_add(origin.y), cell)
        }))
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        let pos = self.inner.get_cursor_position()?;
        Ok(self.unoffset(pos))
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let pos = position.into();
        self.inner.set_cursor_position(self.offset(pos))
    }

    fn clear(&mut self) -> io::Result<()> {
        self.clear_viewport()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        match clear_type {
            ClearType::All => self.clear_viewport(),
            _ => self.clear_viewport(),
        }
    }

    fn size(&self) -> io::Result<Size> {
        Ok(self.viewport.size)
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        let mut ws = self.inner.window_size()?;
        ws.columns_rows = self.viewport.size;
        Ok(ws)
    }

    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.inner)
    }
}
