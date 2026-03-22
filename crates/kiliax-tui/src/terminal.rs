use std::io::{self, Stdout, Write};

use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;
use crossterm::terminal::ScrollUp;
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
    pub fn screen_size(&mut self) -> anyhow::Result<Size> {
        self.full_size = self.terminal.backend_mut().refresh_full_size()?;
        Ok(self.full_size)
    }

    pub fn queue_history_lines(&mut self, lines: Vec<ratatui::text::Line<'static>>) {
        self.pending_history_lines.extend(lines);
    }

    pub fn draw(
        &mut self,
        viewport_height: u16,
        draw_fn: impl FnOnce(&mut ratatui::Frame),
    ) -> anyhow::Result<()> {
        self.full_size = self.terminal.backend_mut().refresh_full_size()?;
        let screen_height = self.full_size.height.max(1);
        let desired_height = viewport_height.clamp(1, screen_height);

        let mut next_viewport = self.viewport;
        next_viewport.x = 0;
        next_viewport.width = self.full_size.width;
        next_viewport.height = desired_height;

        // If expanding the viewport would exceed the screen, scroll the terminal up to make room.
        if next_viewport.y.saturating_add(next_viewport.height) > screen_height {
            let overflow = next_viewport
                .y
                .saturating_add(next_viewport.height)
                .saturating_sub(screen_height);
            if overflow > 0 {
                execute!(io::stdout(), ScrollUp(overflow))?;
                next_viewport.y = next_viewport.y.saturating_sub(overflow);
            }
        }

        // Clamp the viewport to the screen.
        if next_viewport.y >= screen_height {
            next_viewport.y = screen_height.saturating_sub(1);
        }
        if next_viewport.y.saturating_add(next_viewport.height) > screen_height {
            next_viewport.y = screen_height.saturating_sub(next_viewport.height);
        }

        if next_viewport != self.viewport {
            // Clear the old viewport area to prevent stale UI artifacts.
            self.terminal.backend_mut().set_viewport(self.viewport);
            self.terminal.clear()?;

            self.viewport = next_viewport;
            self.terminal.backend_mut().set_viewport(self.viewport);
            self.terminal.clear()?;
        } else {
            self.terminal.backend_mut().set_viewport(self.viewport);
        }

        if !self.pending_history_lines.is_empty() {
            let lines = std::mem::take(&mut self.pending_history_lines);
            let before = self.viewport;
            let backend = self.terminal.backend_mut();
            history::insert_history_lines(backend, &lines, &mut self.viewport, self.full_size)?;
            if self.viewport != before {
                self.terminal.backend_mut().set_viewport(self.viewport);
                self.terminal.clear()?;
            }
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

    let full_size = terminal.backend_mut().refresh_full_size()?;
    let (_, cursor_y) = crossterm::cursor::position().unwrap_or((0, 0));
    let viewport_y = cursor_y.min(full_size.height.saturating_sub(1));
    let viewport = Rect::new(
        0,
        viewport_y,
        full_size.width,
        1.min(full_size.height.max(1)),
    );
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

pub struct ViewportBackend<B>
where
    B: Backend + Write,
{
    inner: B,
    viewport: Viewport,
    full_size: Size,
}

impl ViewportBackend<CrosstermBackend<Stdout>> {
    fn new(inner: CrosstermBackend<Stdout>) -> Self {
        Self {
            inner,
            viewport: Viewport {
                origin: Position { x: 0, y: 0 },
                size: Size::new(0, 0),
            },
            full_size: Size::new(0, 0),
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

    pub fn refresh_full_size(&mut self) -> io::Result<Size> {
        self.full_size = self.inner.size()?;
        Ok(self.full_size)
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
        let max_x = self.full_size.width.saturating_sub(1);
        let max_y = self.full_size.height.saturating_sub(1);
        self.inner.draw(content.filter_map(move |(x, y, cell)| {
            let gx = x.saturating_add(origin.x);
            let gy = y.saturating_add(origin.y);

            // Avoid writing the bottom-right cell: several terminals can scroll the screen when the
            // cursor advances past it, which manifests as extra blank lines in scrollback.
            if gx == max_x && gy == max_y {
                None
            } else {
                Some((gx, gy, cell))
            }
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

impl<B> Write for ViewportBackend<B>
where
    B: Backend + Write,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        std::io::Write::flush(&mut self.inner)
    }
}
