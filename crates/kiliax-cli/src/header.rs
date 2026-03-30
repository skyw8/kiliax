use std::path::Path;

use ratatui::style::Stylize;
use ratatui::text::{Line, Span};

pub fn startup_lines(version: &str, model_id: &str, cwd: &Path) -> Vec<Line<'static>> {
    let cwd = cwd.display().to_string();

    let title = Line::from(vec![
        Span::from(">_ ").dim(),
        Span::from("Kiliax").bold().magenta(),
        Span::from(format!(" (v{version})")).dim(),
    ]);

    let model = Line::from(vec![
        Span::from("model: ").dim(),
        Span::from(model_id.to_string()).cyan(),
    ]);

    let cwd = Line::from(vec![Span::from("cwd: ").dim(), Span::from(cwd)]);

    vec![title, model, cwd, Line::from("")]
}
