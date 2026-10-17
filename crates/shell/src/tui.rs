//! Minimal terminal UI: one prompt, one answer pane. Esc quits, Ctrl-S speaks the answer.
use crate::{mode_name, Axiom};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::DefaultTerminal;

pub fn run(ax: &Axiom) -> Result<()> {
    let mut term = ratatui::init();
    let r = event_loop(&mut term, ax);
    ratatui::restore();
    r
}

fn event_loop(term: &mut DefaultTerminal, ax: &Axiom) -> Result<()> {
    let mut input = String::new();
    let mut out = String::from("Ask anything about your files. Enter runs, Ctrl-S speaks the answer, Esc quits.");
    loop {
        term.draw(|f| {
            let [top, body] = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(f.area());
            f.render_widget(Paragraph::new(input.as_str()).block(Block::bordered().title("axiom")), top);
            f.render_widget(Paragraph::new(out.as_str()).block(Block::bordered().title("answer")).wrap(Wrap { trim: false }), body);
        })?;
        let Event::Key(k) = event::read()? else { continue };
        if k.kind != KeyEventKind::Press {
            continue;
        }
        match k.code {
            KeyCode::Esc => return Ok(()),
            KeyCode::Char('s') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Err(e) = ax.speak(&out) {
                    out = format!("error: {e}");
                }
            }
            KeyCode::Enter => {
                out = match ax.ask(&input, platform::now(), None, false) {
                    Ok((r, t)) => format!("[{}, confidence {:.2}, tags {:?}]\n{t}", mode_name(&r.mode), r.plan.confidence, r.plan.tags),
                    Err(e) => format!("error: {e}"),
                };
            }
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Char(c) => input.push(c),
            _ => {}
        }
    }
}
