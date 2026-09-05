use jiff::{Timestamp, tz::TimeZone};
use ratatui::{
    Frame, layout,
    style::{self, Stylize},
    text, widgets,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{
    super::app::{clock, format_limit},
    duration,
    input::Input,
    state::{self, App, Failure, Field, Message, Picker},
};

pub(super) const MIN_WIDTH: u16 = 32;
pub(super) const MIN_HEIGHT: u16 = 10;

pub(super) fn draw(frame: &mut Frame, app: &mut App, now: Timestamp, zone: &TimeZone) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        frame.render_widget(
            widgets::Paragraph::new("Resize to at least 32 × 10.\nq: quit; timers keep running")
                .wrap(widgets::Wrap { trim: false }),
            area,
        );
        return;
    }
    let compact = area.width < 60;
    let (feedback, feedback_style) = feedback(app);
    let feedback_height = if feedback.is_empty() && app.picker.is_none() {
        0
    } else {
        2
    };
    let [header, body, message, hints] = layout::Layout::vertical([
        layout::Constraint::Length(1),
        layout::Constraint::Min(1),
        layout::Constraint::Length(feedback_height),
        layout::Constraint::Length(1),
    ])
    .areas(area);
    let heading = format!(
        "tt  ·  {}  {}",
        now.to_zoned(zone.clone()).strftime("%a %d %b %H:%M"),
        zone.iana_name().unwrap_or("local")
    );
    frame.render_widget(widgets::Paragraph::new(heading).dim(), header);
    frame.render_widget(
        widgets::Paragraph::new(clean(feedback))
            .style(feedback_style)
            .wrap(widgets::Wrap { trim: false }),
        message,
    );
    frame.render_widget(
        widgets::Paragraph::new(shortcuts(app, compact)).dim(),
        hints,
    );

    // Small splits use the whole body for forms, rather than nesting cramped boxes.
    if let Some(picker) = &mut app.picker
        && (compact || area.height < 22)
    {
        draw_picker(
            frame,
            picker,
            app.error.as_ref(),
            body,
            false,
            app.refresh_error.is_some(),
        );
        return;
    }
    draw_dashboard(frame, app, now, zone, body, compact);
    if let Some(picker) = &mut app.picker {
        frame
            .buffer_mut()
            .set_style(body, style::Style::new().dim());
        let width = body.width.saturating_sub(4).min(76);
        let popup = layout::Rect::new(
            body.x + (body.width - width) / 2,
            body.y + (body.height - 16) / 2,
            width,
            16,
        );
        draw_picker(
            frame,
            picker,
            app.error.as_ref(),
            popup,
            true,
            app.refresh_error.is_some(),
        );
    }
}

fn draw_dashboard(
    frame: &mut Frame,
    app: &mut App,
    now: Timestamp,
    zone: &TimeZone,
    area: layout::Rect,
    compact: bool,
) {
    let mut earnings = app
        .snapshot
        .today
        .earnings_micros
        .iter()
        .map(|(currency, micros)| money(currency, *micros))
        .collect::<Vec<_>>();
    if earnings.is_empty() {
        earnings.push("—".into());
    }
    let mut retainer_height = 0;
    if !app.snapshot.retainers.is_empty() {
        retainer_height = u16::try_from(app.snapshot.retainers.len())
            .unwrap_or(u16::MAX)
            .saturating_mul(2)
            .saturating_add(1)
            .min(area.height / 2)
            .min(area.height.saturating_sub(5));
    }
    let gap = u16::from(area.height >= 10);
    let summary_height = (earnings.len() as u16 + 1 + gap)
        .min(area.height.saturating_sub(4 + retainer_height))
        .max(2 + gap);
    let [summary, title, timers, retainers] = layout::Layout::vertical([
        layout::Constraint::Length(summary_height),
        layout::Constraint::Length(1),
        layout::Constraint::Min(2),
        layout::Constraint::Length(retainer_height),
    ])
    .areas(area);
    draw_retainers(frame, &app.snapshot.retainers, retainers);
    let visible = usize::from(summary_height - 1 - gap);
    if earnings.len() > visible {
        let hidden = earnings.len() - visible + 1;
        earnings.truncate(visible);
        earnings[visible - 1] = format!("+{hidden} currencies; resize");
    }
    let worked = hours(app.snapshot.today.worked_us);
    if compact {
        let mut lines = vec![text::Line::from(vec![
            text::Span::raw("Worked today  "),
            text::Span::raw(worked).bold(),
        ])];
        lines.extend(earnings.iter().enumerate().map(|(i, earned)| {
            let label = if i == 0 {
                "Earned today  "
            } else {
                "              "
            };
            text::Line::from(vec![text::Span::raw(label), text::Span::raw(earned).bold()])
        }));
        frame.render_widget(widgets::Paragraph::new(lines), summary);
    } else {
        let [work, cash] =
            layout::Layout::horizontal([layout::Constraint::Percentage(50); 2]).areas(summary);
        frame.render_widget(
            widgets::Paragraph::new(vec![
                text::Line::from("Worked today").dim(),
                text::Line::from(worked).bold(),
            ]),
            work,
        );
        let mut lines = vec![text::Line::from("Earned today").dim()];
        lines.extend(earnings.into_iter().map(|s| text::Line::from(s).bold()));
        frame.render_widget(widgets::Paragraph::new(lines), cash);
    }
    frame.render_widget(
        widgets::Paragraph::new(format!("Running · {}", app.snapshot.timers.len())).dim(),
        title,
    );
    if app.snapshot.timers.is_empty() {
        let mut empty = "No running timers. Press s to start work.";
        if app.snapshot.projects.is_empty() {
            empty = "No active projects. Set up in the CLI:\ntt client add --name <client>\ntt project add --help\nNew projects appear automatically.";
        }
        frame.render_widget(
            widgets::Paragraph::new(empty).wrap(widgets::Wrap { trim: false }),
            timers,
        );
        return;
    }
    let width = usize::from(timers.width.saturating_sub(2));
    let items = app.snapshot.timers.iter().map(|status| {
        let timer = &status.timer;
        let project = app
            .snapshot
            .projects
            .iter()
            .find(|p| p.agreement.id() == timer.agreement())
            .expect("running timers have active projects");
        let elapsed = duration(i128::from(
            now.as_microsecond() - timer.started_at().as_microsecond(),
        ));
        let mut metadata = format!(
            "{} / {}",
            clean(project.client.name()),
            clean(project.project.name())
        );
        if !compact {
            metadata.push_str(&format!(" · {}", clean(project.agreement.name())));
        }
        if area.width >= 90 {
            metadata.push_str(&format!(" · since {}", clock(timer.started_at(), zone)));
        }
        let mut lines = vec![columns(timer.scope(), text::Span::raw(elapsed), width).bold()];
        let mut cap = text::Span::raw("");
        if let Some(limit) = status.limit {
            let label = format!("{} left", format_limit(Some(limit)));
            cap = text::Span::raw(label);
            if limit.is_reached() {
                cap = text::Span::raw("LIMIT REACHED").red();
            } else if limit.remaining_us() <= 3_600_000_000 {
                cap = cap.yellow();
            }
        }
        if compact && status.limit.is_some() && timers.height >= 3 {
            lines.push(text::Line::from(truncate(&metadata, width)));
            lines.push(text::Line::from(cap));
        } else {
            lines.push(columns(&metadata, cap, width));
        }
        widgets::ListItem::new(lines)
    });
    frame.render_stateful_widget(
        widgets::List::new(items)
            .highlight_symbol("› ")
            .highlight_style(selection()),
        timers,
        &mut app.selection,
    );
}

fn draw_retainers(frame: &mut Frame, totals: &[tt::model::RetainerMonth], area: layout::Rect) {
    if area.height == 0 {
        return;
    }
    let visible = usize::from(area.height.saturating_sub(1) / 2);
    let mut title = "Retainers · monthly estimate".to_owned();
    if visible < totals.len() {
        title = format!("Retainers · {visible}/{} · resize", totals.len());
    }
    let mut lines = vec![text::Line::from(title).dim()];
    for total in totals.iter().take(visible) {
        let currency = total.agreement.pricing().currency();
        let mut label = format!("{} #{}", total.project.name(), total.project.id());
        if area.width >= 60 {
            label = format!(
                "{} / {} · {}",
                total.client.name(),
                total.project.name(),
                total.agreement.name()
            );
        }
        lines.push(
            columns(
                &label,
                text::Span::raw(money(currency, total.charge_micros())),
                usize::from(area.width),
            )
            .bold(),
        );
        let mut detail = format!(
            "{} · {} / {} incl",
            total.month.strftime("%b %Y"),
            hours(total.worked_us),
            hours(total.included_us),
        );
        if area.width >= 60 {
            detail.push_str(&format!(
                " · {} overage",
                money(currency, total.overage_micros)
            ));
        }
        lines.push(text::Line::from(truncate(&detail, usize::from(area.width))).dim());
    }
    frame.render_widget(widgets::Paragraph::new(lines), area);
}

fn hours(micros: i128) -> String {
    let minutes = micros / 60_000_000;
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

fn money(currency: &str, micros: i128) -> String {
    let cents = (micros + 5_000) / 10_000;
    format!("{currency} {}.{:02}", cents / 100, cents % 100)
}

fn draw_picker(
    frame: &mut Frame,
    picker: &mut Picker,
    error: Option<&Failure>,
    area: layout::Rect,
    bordered: bool,
    read_only: bool,
) {
    frame.render_widget(widgets::Clear, area);
    let mut inner = area;
    if bordered {
        let block = widgets::Block::bordered()
            .border_type(widgets::BorderType::Rounded)
            .title(" Start / switch ");
        inner = block.inner(area);
        frame.render_widget(block, area);
    }
    match picker {
        Picker::Project {
            projects,
            query,
            selection: selected,
        } => {
            let error_height = if error.is_some() { 2 } else { 0 };
            let [input, error_area, list] = layout::Layout::vertical([
                layout::Constraint::Length(2),
                layout::Constraint::Length(error_height),
                layout::Constraint::Min(1),
            ])
            .areas(inner);
            input_field(
                frame,
                "Project search",
                query,
                "Type to search",
                input,
                true,
            );
            draw_error(frame, error, error_area);
            let items = projects
                .matches()
                .map(|p| {
                    widgets::ListItem::new(truncate(
                        &format!(
                            "{} / {} · {}",
                            clean(p.client.name()),
                            clean(p.project.name()),
                            clean(p.agreement.name())
                        ),
                        usize::from(list.width.saturating_sub(2)),
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
                        .highlight_style(selection()),
                    list,
                    selected,
                );
            }
        }
        Picker::Scope {
            project,
            previous,
            recent,
            input,
            selection: selected,
            at,
            field,
        } => {
            let context_height = u16::from(inner.height >= 10);
            let field_height = if inner.height < 8 { 1 } else { 2 };
            let error_height = if error.is_some() { 2 } else { 0 };
            let scope_error = if error.is_some_and(|e| e.field == Some(Field::Scope)) {
                error_height
            } else {
                0
            };
            let time_error = error_height - scope_error;
            let choices = state::scopes(recent, input.as_str());
            let list_height = (choices.len() as u16).min(4).min(
                inner
                    .height
                    .saturating_sub(context_height + error_height + field_height * 2 + 1),
            );
            let [
                name,
                scope,
                scope_message,
                list,
                time,
                time_message,
                _,
                action,
            ] = layout::Layout::vertical([
                layout::Constraint::Length(context_height),
                layout::Constraint::Length(field_height),
                layout::Constraint::Length(scope_error),
                layout::Constraint::Length(list_height),
                layout::Constraint::Length(field_height),
                layout::Constraint::Length(time_error),
                layout::Constraint::Min(0),
                layout::Constraint::Length(1),
            ])
            .areas(inner);
            frame.render_widget(
                widgets::Paragraph::new(truncate(
                    &format!(
                        "{} / {}",
                        clean(project.client.name()),
                        clean(project.project.name())
                    ),
                    usize::from(name.width),
                ))
                .bold(),
                name,
            );
            let placeholder = selected
                .selected()
                .and_then(|i| choices.get(i))
                .copied()
                .unwrap_or("Type a scope");
            input_field(
                frame,
                "Scope",
                input,
                placeholder,
                scope,
                *field == Field::Scope,
            );
            let items = choices.into_iter().map(|s| {
                widgets::ListItem::new(truncate(s, usize::from(list.width.saturating_sub(2))))
            });
            frame.render_stateful_widget(
                widgets::List::new(items)
                    .highlight_symbol("› ")
                    .highlight_style(selection()),
                list,
                selected,
            );
            input_field(
                frame,
                "Time · blank = now",
                at,
                "now",
                time,
                *field == Field::Time,
            );
            draw_error(frame, error, scope_message);
            draw_error(frame, error, time_message);
            let mut label = "Enter: Start".to_owned();
            if let Some(timer) = previous {
                label = format!("Enter: Switch from {}", clean(timer.scope()));
            }
            if read_only {
                label = "Waiting for database…".into();
            }
            frame.render_widget(
                widgets::Paragraph::new(truncate(&label, usize::from(action.width))).bold(),
                action,
            );
        }
    }
}

fn input_field(
    frame: &mut Frame,
    label: &str,
    input: &Input,
    placeholder: &str,
    area: layout::Rect,
    focused: bool,
) {
    if area.height == 0 {
        return;
    }
    let mut label = label.to_owned();
    if area.height == 1 {
        label = format!("{}: ", label.split('·').next().unwrap_or(&label).trim());
    }
    let mut heading = text::Line::from(label.clone()).dim();
    if focused {
        heading = text::Line::from(format!("› {label}")).bold();
    }
    let [title, value] = if area.height == 1 {
        layout::Layout::horizontal([
            layout::Constraint::Length(heading.width() as u16),
            layout::Constraint::Min(1),
        ])
        .areas(area)
    } else {
        layout::Layout::vertical([layout::Constraint::Length(1), layout::Constraint::Min(0)])
            .areas(area)
    };
    frame.render_widget(widgets::Paragraph::new(heading), title);
    let (window, column) = input.window(value.width);
    let mut line = text::Line::from(window);
    if input.as_str().is_empty() {
        line = text::Line::from(truncate(placeholder, usize::from(value.width))).dim();
    }
    frame.render_widget(widgets::Paragraph::new(line), value);
    if focused && value.width > 0 && value.height > 0 {
        frame.set_cursor_position((value.x + column, value.y));
    }
}

fn draw_error(frame: &mut Frame, error: Option<&Failure>, area: layout::Rect) {
    if let Some(error) = error {
        frame.render_widget(
            widgets::Paragraph::new(format!("Error: {}", clean(&error.text)))
                .red()
                .wrap(widgets::Wrap { trim: false }),
            area,
        );
    }
}

fn feedback(app: &App) -> (&str, style::Style) {
    if let Some(error) = &app.refresh_error {
        return (error, style::Style::new().yellow());
    }
    if app.picker.is_none()
        && let Some(error) = &app.error
    {
        return (&error.text, style::Style::new().red());
    }
    match &app.message {
        Some(message) => {
            let mut style = style::Style::new();
            if matches!(message, Message::Warning(_)) {
                style = style.yellow();
            }
            (message.text(), style)
        }
        None => ("", style::Style::new()),
    }
}

fn shortcuts(app: &App, compact: bool) -> &'static str {
    if app.refresh_error.is_some() {
        if app.picker.is_some() {
            return "Read-only  Esc back  Ctrl-C quit";
        }
        return "Refresh failed · read-only q quit";
    }
    match &app.picker {
        Some(Picker::Project { .. }) if compact => "↑↓ choose  Enter next  Esc back",
        Some(Picker::Project { .. }) => "↑↓ choose   Enter next   Esc cancel   Ctrl-C quit",
        Some(Picker::Scope { .. }) if compact => "↑↓ recent  Tab field  Esc back",
        Some(Picker::Scope { .. }) => {
            "↑↓ recent   Tab field   Enter submit   Esc cancel   Ctrl-C quit"
        }
        None if app.error.is_some() || app.message.is_some() => "s new x stop Esc dismiss q quit",
        None if app.snapshot.projects.is_empty() => "CLI setup required  q quit",
        None if compact => "↑↓ select  s new  x stop  q quit",
        None => "↑↓/jk select   s start/switch   x stop   q quit",
    }
}

fn selection() -> style::Style {
    // Explicit paired RGB colors keep selection legible in light and dark themes.
    style::Style::new()
        .fg(style::Color::Rgb(18, 22, 28))
        .bg(style::Color::Rgb(232, 236, 240))
}

fn columns(left: &str, right: text::Span<'static>, width: usize) -> text::Line<'static> {
    let gap = usize::from(!right.content.is_empty()) * 2;
    let left = truncate(left, width.saturating_sub(right.content.width() + gap));
    let padding = width.saturating_sub(left.width() + right.content.width());
    text::Line::from(vec![
        text::Span::raw(left),
        text::Span::raw(" ".repeat(padding)),
        right,
    ])
}

fn truncate(value: &str, width: usize) -> String {
    let value = clean(value);
    if value.width() <= width {
        return value;
    }
    if width == 0 {
        return String::new();
    }
    let mut used = 0;
    let mut clipped = String::new();
    for g in value.graphemes(true) {
        used += g.width();
        if used >= width {
            break;
        }
        clipped.push_str(g);
    }
    clipped.push('…');
    clipped
}

fn clean(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
