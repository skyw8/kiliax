use std::path::Path;

use ratatui::style::Stylize;
use ratatui::text::{Line, Span};

pub fn startup_lines(version: &str, model_id: &str, cwd: &Path) -> Vec<Line<'static>> {
    let cwd = cwd.display().to_string();

    let line = Line::from(vec![
        Span::from(">_ ").dim(),
        Span::from("Kiliax").bold().magenta(),
        Span::from(format!(" (v{version})")).dim(),
        Span::from("  ").dim(),
        Span::from("model: ").dim(),
        Span::from(model_id.to_string()).cyan(),
        Span::from("  ").dim(),
        Span::from("cwd: ").dim(),
        Span::from(cwd),
    ]);

    vec![line, Line::from("")]
}
