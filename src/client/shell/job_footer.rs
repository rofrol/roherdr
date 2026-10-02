//! Client-only chrome. Never written to the pane terminal or its scrollback.
use super::*;

pub(super) fn render(state: &ClientShellState, buffer: &mut Buffer, area: Rect) {
    let Some(job) = state.active_job_metadata() else {
        return;
    };
    let palette = &state.config.palette;
    let style = Style::default().fg(palette.text).bg(palette.sidebar_bg);
    buffer.set_style(area, style);
    for x in area.x..area.right() {
        if let Some(cell) = buffer.cell_mut((x, area.y)) {
            cell.set_symbol(" ");
        }
    }
    let tab = state.snapshot.as_deref().and_then(|s| {
        s.tabs
            .iter()
            .find(|t| Some(&t.tab_id) == s.focused_tab_id.as_ref())
    });
    let status = match tab.and_then(|t| t.status) {
        Some(crate::api::schema::TabStatus::Running) => crate::ui::motion::job_glyph(),
        Some(crate::api::schema::TabStatus::Succeeded) => "✓",
        Some(crate::api::schema::TabStatus::Failed) => "✗",
        _ => "·",
    };
    // A running job that does nothing says so, with a ring that does not turn.
    let idle = tab.is_some_and(|t| {
        t.status == Some(crate::api::schema::TabStatus::Running)
            && t.activity == Some(crate::api::schema::TabActivity::Idle)
    });
    let status = if idle { "◌" } else { status };
    let text = format!(
        "{status} {}{} — {}",
        job.name,
        if idle { " (idle)" } else { "" },
        job.why.as_deref().unwrap_or(&job.origin)
    );
    let text: String = text.chars().filter(|c| !c.is_control()).collect();
    buffer.set_stringn(area.x, area.y, " ← ", 3, style);
    buffer.set_stringn(
        area.x.saturating_add(3),
        area.y,
        text,
        usize::from(area.width.saturating_sub(6)),
        style,
    );
    buffer.set_stringn(area.right().saturating_sub(3), area.y, " × ", 3, style);
}
