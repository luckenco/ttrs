use jiff::{Timestamp, tz::TimeZone};
use ratatui::{
    Frame, layout,
    style::{Color, Style, Stylize},
    text, widgets,
};

use super::{
    super::app::{clock, format_limit},
    duration,
    state::{self, App, Field, Picker},
};

pub(super) fn draw(frame: &mut Frame, app: &mut App, now: Timestamp, zone: &TimeZone) {
    let area = frame.area();
    if area.width < 50 || area.height < 16 {
        frame.render_widget(
            widgets::Paragraph::new("Resize to at least 50 x 16.\nq: quit (timers keep running)")
                .wrap(widgets::Wrap { trim: false }),
            area,
        );
        return;
    }
    let earnings = app
        .snapshot
        .today
        .earnings_micros
        .iter()
        .map(|(currency, micros)| {
            let cents = (micros + 5_000) / 10_000;
            format!("{currency} {}.{:02}", cents / 100, cents % 100)
        })
        .collect::<Vec<_>>();
    let summary_height = (earnings.len() as u16 + 2).max(3).min(area.height - 10);
    let [header, summary, timers, message, hints] = layout::Layout::vertical([
        layout::Constraint::Length(1),
        layout::Constraint::Length(summary_height),
        layout::Constraint::Min(3),
        layout::Constraint::Length(2),
        layout::Constraint::Length(1),
    ])
    .areas(area);
    let heading = format!(
        " tt  |  {}  {}",
        now.to_zoned(zone.clone()).strftime("%a %d %b %H:%M:%S"),
        zone.iana_name().unwrap_or("device timezone")
    );
    frame.render_widget(widgets::Paragraph::new(heading).bold(), header);
    let [work, cash] =
        layout::Layout::horizontal([layout::Constraint::Percentage(50); 2]).areas(summary);
    frame.render_widget(
        widgets::Paragraph::new(duration(app.snapshot.today.worked_us))
            .bold()
            .block(block(" Worked today ")),
        work,
    );
    let visible = usize::from(summary_height - 2);
    let mut earned = earnings.join("\n");
    if earnings.len() > visible {
        earned = earnings[..visible - 1].join("\n");
        earned.push_str(&format!(
            "\n+{} currencies; resize",
            earnings.len() - visible + 1
        ));
    }
    if earnings.is_empty() {
        earned = "—".into();
    }
    frame.render_widget(
        widgets::Paragraph::new(earned)
            .fg(Color::Green)
            .block(block(" Earned today ")),
        cash,
    );

    if app.snapshot.timers.is_empty() {
        let mut empty = "No running timers. Press s to start work.".to_owned();
        if app.snapshot.projects.is_empty() {
            empty = "No active projects. Set up using the CLI:\n\ntt client add --name <client>\ntt project add --help\n\nProjects appear automatically after creation.".into();
        }
        frame.render_widget(
            widgets::Paragraph::new(empty)
                .wrap(widgets::Wrap { trim: false })
                .block(block(" Running ")),
            timers,
        );
    } else {
        let items = app.snapshot.timers.iter().map(|status| {
            let timer = &status.timer;
            let project = app
                .snapshot
                .projects
                .iter()
                .find(|p| p.agreement.id() == timer.agreement())
                .expect("running timers have active projects");
            let mut cap_style = Style::default();
            if let Some(limit) = status.limit {
                if limit.is_reached() {
                    cap_style = Style::default().fg(Color::Red);
                } else if limit.remaining_us() <= 3_600_000_000 {
                    cap_style = Style::default().fg(Color::Yellow);
                }
            }
            widgets::ListItem::new(vec![
                text::Line::from(clean(timer.scope())).bold(),
                text::Line::from(format!(
                    "{} / {} · {}",
                    clean(project.client.name()),
                    clean(project.project.name()),
                    clean(project.agreement.name())
                ))
                .dim(),
                text::Line::from(vec![
                    text::Span::raw(format!(
                        "{}  since {}  ",
                        duration(i128::from(
                            now.as_microsecond() - timer.started_at().as_microsecond()
                        )),
                        clock(timer.started_at(), zone)
                    )),
                    text::Span::styled(format!("cap: {}", format_limit(status.limit)), cap_style),
                ]),
                text::Line::default(),
            ])
        });
        frame.render_stateful_widget(
            widgets::List::new(items)
                .block(block(" Running "))
                .highlight_symbol("› ")
                .highlight_style(Style::default().bg(Color::DarkGray)),
            timers,
            &mut app.selection,
        );
    }
    frame.render_widget(
        widgets::Paragraph::new(clean(&app.message))
            .wrap(widgets::Wrap { trim: false })
            .fg(Color::Yellow),
        message,
    );
    frame.render_widget(
        widgets::Paragraph::new(" ↑↓/jk select   s start/switch   x stop   q quit").dim(),
        hints,
    );
    if let Some(picker) = &mut app.picker {
        draw_picker(frame, picker);
    }
}

fn draw_picker(frame: &mut Frame, picker: &mut Picker) {
    let area = frame.area();
    let width = area.width.saturating_sub(4).min(80);
    let height = area.height.saturating_sub(4).min(16);
    let popup = layout::Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(widgets::Clear, popup);
    let border = block(" Start / switch ").border_style(Style::default().fg(Color::Cyan));
    let inner = border.inner(popup);
    frame.render_widget(border, popup);
    match picker {
        Picker::Project {
            projects,
            query,
            selection,
        } => {
            let [input, list, hints] = layout::Layout::vertical([
                layout::Constraint::Length(3),
                layout::Constraint::Min(1),
                layout::Constraint::Length(1),
            ])
            .areas(inner);
            input_field(frame, " Project search ", query, input, true);
            let matches = state::matching_projects(projects, query);
            let items = matches
                .iter()
                .map(|p| {
                    widgets::ListItem::new(format!(
                        "{} / {} · {}",
                        clean(p.client.name()),
                        clean(p.project.name()),
                        clean(p.agreement.name())
                    ))
                })
                .collect::<Vec<_>>();
            if items.is_empty() {
                frame.render_widget(
                    widgets::Paragraph::new("No matching active projects."),
                    list,
                );
            } else {
                frame.render_stateful_widget(
                    widgets::List::new(items)
                        .highlight_symbol("› ")
                        .highlight_style(Style::default().bg(Color::DarkGray)),
                    list,
                    selection,
                );
            }
            frame.render_widget(
                widgets::Paragraph::new("↑↓ select · Enter choose · Esc cancel").dim(),
                hints,
            );
        }
        Picker::Scope {
            project,
            previous,
            recent,
            input,
            selection,
            at,
            field,
        } => {
            let [name, scope, list, time, action] = layout::Layout::vertical([
                layout::Constraint::Length(1),
                layout::Constraint::Length(3),
                layout::Constraint::Min(1),
                layout::Constraint::Length(3),
                layout::Constraint::Length(1),
            ])
            .areas(inner);
            frame.render_widget(
                widgets::Paragraph::new(format!(
                    "{} / {}",
                    clean(project.client.name()),
                    clean(project.project.name())
                ))
                .bold(),
                name,
            );
            input_field(
                frame,
                " Scope (type new or select recent) ",
                input,
                scope,
                *field == Field::Scope,
            );
            let choices = state::scopes(recent, input)
                .into_iter()
                .map(|s| widgets::ListItem::new(clean(s)));
            frame.render_stateful_widget(
                widgets::List::new(choices)
                    .highlight_symbol("› ")
                    .highlight_style(Style::default().bg(Color::DarkGray)),
                list,
                selection,
            );
            input_field(
                frame,
                " Time (blank = now; Tab to edit) ",
                at,
                time,
                *field == Field::Time,
            );
            let mut label = "Enter: Start".to_owned();
            if let Some(timer) = previous {
                label = format!("Enter: Switch from {}", clean(timer.scope()));
            }
            frame.render_widget(
                widgets::Paragraph::new(format!("{label} · Esc cancel")).fg(Color::Cyan),
                action,
            );
        }
    }
}

fn input_field(frame: &mut Frame, title: &str, value: &str, area: layout::Rect, focused: bool) {
    let mut border = block(title);
    if focused {
        border = border.border_style(Style::default().fg(Color::Cyan));
    }
    let inner = border.inner(area);
    let value = text::Line::from(clean(value));
    let width = value.width();
    let scroll = width.saturating_sub(usize::from(inner.width.saturating_sub(1)));
    frame.render_widget(
        widgets::Paragraph::new(value)
            .scroll((0, scroll.min(u16::MAX as usize) as u16))
            .block(border),
        area,
    );
    if focused && inner.width > 0 && inner.height > 0 {
        frame.set_cursor_position((inner.x + (width - scroll) as u16, inner.y));
    }
}

fn block(title: &str) -> widgets::Block<'_> {
    widgets::Block::bordered()
        .border_type(widgets::BorderType::Rounded)
        .title(title)
}

fn clean(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
