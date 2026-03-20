//! Orchestration timeline widget for the session TUI.

use nca_common::team::{TeamEvent, TeamStatus, WorkerStatus};
use ratatui::prelude::*;
use ratatui::widgets::*;

/// A scrollable timeline of multi-agent orchestration events with a status bar.
pub struct OrchestrationTimeline {
    pub events: Vec<TeamEvent>,
    pub status: Option<TeamStatus>,
    pub scroll_offset: usize,
}

impl OrchestrationTimeline {
    pub fn new() -> Self {
        Self {
            events: vec![],
            status: None,
            scroll_offset: 0,
        }
    }

    pub fn push_event(&mut self, event: TeamEvent) {
        self.events.push(event);
    }

    pub fn set_status(&mut self, status: TeamStatus) {
        self.status = Some(status);
    }

    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    pub fn scroll_down(&mut self, lines: usize) {
        let max = self.events.len().saturating_sub(1);
        self.scroll_offset = (self.scroll_offset + lines).min(max);
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .title(" Orchestration Timeline ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Status bar at top
        if let Some(status) = &self.status {
            let agent_count = status.workers.len();
            let working = status
                .workers
                .values()
                .filter(|s| matches!(s, WorkerStatus::Working))
                .count();
            let phase_text = format!(
                " Phase: {:?} | Agents: {} ({} active) | Cost: ${:.4} ",
                status.phase, agent_count, working, status.cost.total_usd
            );
            let status_line = Line::from(phase_text).style(Style::default().fg(Color::Cyan));
            buf.set_line(inner.x, inner.y, &status_line, inner.width);
        }

        // Events list below the status bar
        let events_area = Rect {
            y: inner.y + 1,
            height: inner.height.saturating_sub(1),
            ..inner
        };

        if events_area.height == 0 {
            return;
        }

        let items: Vec<ListItem> = self
            .events
            .iter()
            .skip(self.scroll_offset)
            .map(|e| {
                let ts = e.timestamp.format("%H:%M:%S");
                let color = match e.source_agent.as_str() {
                    "orchestrator" => Color::Yellow,
                    _ => Color::Green,
                };
                let line = Line::from(vec![
                    Span::styled(
                        format!("[{ts}] "),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("[{}] ", e.source_agent),
                        Style::default().fg(color),
                    ),
                    Span::raw(format!("{:?}", e.event)),
                ]);
                ListItem::new(line)
            })
            .collect();

        let list = List::new(items);
        Widget::render(list, events_area, buf);
    }
}

impl Default for OrchestrationTimeline {
    fn default() -> Self {
        Self::new()
    }
}
