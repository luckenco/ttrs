use jiff::{Timestamp, tz::TimeZone};
use ratatui::{
    Frame,
    layout::{self, Constraint, Rect},
    style::Stylize,
    text, widgets,
};

use super::{
    choose::Choose,
    form::{Choice, FieldId, Form, Value},
    state::App,
    view,
    workspace::{Overlay, Section},
};

pub(super) fn header(frame: &mut Frame, app: &App, area: Rect) {
    let primary = Section::PRIMARY.contains(&app.workspace.section);
    if !primary && area.width < 60 {
        frame.render_widget(
            widgets::Paragraph::new(format!("tt · {}  : menu", app.workspace.section.title()))
                .bold(),
            area,
        );
        return;
    }
    let mut spans = Section::PRIMARY
        .into_iter()
        .enumerate()
        .map(|(i, section)| {
            let mut title = section.title();
            if section == Section::Dashboard && area.width < 40 {
                title = "Dash";
            }
            if section == app.workspace.section {
                text::Span::raw(format!(" [{} {}] ", i + 1, title)).style(view::selection())
            } else {
                text::Span::raw(format!(" {} {} ", i + 1, title)).dim()
            }
        })
        .collect::<Vec<_>>();
    if !primary {
        spans.push(text::Span::raw(format!(" · {}", app.workspace.section.title())).bold());
    }
    spans.push(text::Span::raw(" : menu").dim());
    frame.render_widget(widgets::Paragraph::new(text::Line::from(spans)), area);
}

pub(super) fn draw(
    frame: &mut Frame,
    app: &mut App,
    now: Timestamp,
    zone: &TimeZone,
    area: Rect,
) -> bool {
    let read_only = app.refresh_error.is_some();
    if let Some(overlay) = &mut app.workspace.overlay {
        match overlay {
            Overlay::Actions(picker) => {
                let detail = picker
                    .selected()
                    .and_then(|item| item.disabled.clone())
                    .unwrap_or_else(|| "Enter: open action · Esc: back".into());
                choose(
                    frame,
                    picker,
                    area,
                    |item| {
                        if item.disabled.is_some() {
                            format!("{} · unavailable", item.label)
                        } else {
                            item.label.clone()
                        }
                    },
                    &detail,
                );
            }
            Overlay::Entries(picker) => choose(
                frame,
                picker,
                area,
                |entry| super::recovery::entry_label(entry, zone),
                "Enter: review deletion · Tab: details · Esc: back",
            ),
            Overlay::Filter(picker) => choose(
                frame,
                picker,
                area,
                |(_, label)| label.clone(),
                "Enter: apply filter · Esc: back",
            ),
            Overlay::Detail {
                title,
                text,
                scroll,
            } => detail(frame, title, text, scroll, area),
            Overlay::Form(form) => draw_form(frame, form, read_only, area),
        }
        return true;
    }
    if app.workspace.section == Section::Dashboard {
        return false;
    }
    if app.workspace.section == Section::Settings {
        let text = format!(
            "Display timezone\n{}\nSource: {}\n\nLocal time: {}\n\nEnter: change timezone\n\nAffects displayed timestamps, local time input, and daily totals. Project billing timezones do not change.",
            zone.iana_name().unwrap_or("Device timezone (no IANA name)"),
            app.workspace.source,
            now.to_zoned(zone.clone()).strftime("%F %T")
        );
        detail(
            frame,
            "Settings",
            &text,
            &mut app.workspace.settings_scroll,
            area,
        );
        return true;
    }
    let section = app.workspace.section;
    let client_label = app
        .workspace
        .browser()
        .and_then(|b| b.client)
        .map(|id| app.workspace.catalogs.client_name(id).to_owned())
        .unwrap_or_else(|| "All clients".into());
    let browser = app.workspace.browser_mut().expect("catalog section");
    if browser.preview && area.width < 80 {
        let text = browser.selected().map_or_else(
            || "No selected item. Tab: return to list.".into(),
            |row| row.detail.clone(),
        );
        detail(
            frame,
            "Details · Tab: list",
            &text,
            &mut browser.scroll,
            area,
        );
        return true;
    }
    let [filter, query, content] = layout::Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Min(1),
    ])
    .areas(area);
    let mut label = "Current";
    if browser.all {
        label = "All (incl. archived/replaced)";
    }
    if section == Section::Clients {
        label = "Clients";
    }
    frame.render_widget(
        widgets::Paragraph::new(view::truncate(
            &format!("{label} · {client_label}"),
            usize::from(filter.width),
        ))
        .dim(),
        filter,
    );
    view::input_field(
        frame,
        "Search · / to focus",
        &browser.query,
        "All items",
        query,
        browser.searching,
    );
    let mut list = content;
    if content.width >= 80 {
        let [left, gap, right] = layout::Layout::horizontal([
            Constraint::Percentage(45),
            Constraint::Length(2),
            Constraint::Min(1),
        ])
        .areas(content);
        list = left;
        frame.render_widget(
            widgets::Paragraph::new("│\n".repeat(usize::from(gap.height))).dim(),
            gap,
        );
        if let Some(row) = browser.selected() {
            let [body, hint] =
                layout::Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(right);
            let text = row.detail.clone();
            if browser.preview {
                detail(frame, "› Details", &text, &mut browser.scroll, body);
            } else {
                frame.render_widget(
                    widgets::Paragraph::new(view::clean_lines(&text))
                        .wrap(widgets::Wrap { trim: false }),
                    body,
                );
            }
            frame.render_widget(
                widgets::Paragraph::new("Tab: list/details · : actions").dim(),
                hint,
            );
        }
    }
    if browser.rows.matches().len() == 0 {
        let message = if browser.query.as_str().is_empty() {
            "No items in this view.\nn Create · : filters/actions"
        } else {
            "No matches. / Change search\nCheck client and status filters."
        };
        frame.render_widget(
            widgets::Paragraph::new(message).wrap(widgets::Wrap { trim: false }),
            list,
        );
    } else {
        let items = browser.rows.matches().map(|r| {
            widgets::ListItem::new(view::truncate(
                &r.label,
                usize::from(list.width.saturating_sub(2)),
            ))
        });
        frame.render_stateful_widget(
            widgets::List::new(items)
                .highlight_symbol("› ")
                .highlight_style(view::selection()),
            list,
            &mut browser.selection,
        );
    }
    true
}

fn choose<T>(
    frame: &mut Frame,
    picker: &mut Choose<T>,
    area: Rect,
    label: impl Fn(&T) -> String,
    hint: &str,
) {
    let [title, input, list, note] = layout::Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .areas(area);
    frame.render_widget(
        widgets::Paragraph::new(view::truncate(&picker.title, usize::from(title.width))).bold(),
        title,
    );
    view::input_field(
        frame,
        "Search",
        &picker.input,
        "Type to filter",
        input,
        true,
    );
    if picker.items.matches().len() == 0 {
        frame.render_widget(widgets::Paragraph::new("No matches. Esc: back"), list);
    } else {
        let items = picker.items.matches().map(|item| {
            widgets::ListItem::new(view::truncate(
                &label(item),
                usize::from(list.width.saturating_sub(2)),
            ))
        });
        frame.render_stateful_widget(
            widgets::List::new(items)
                .highlight_symbol("› ")
                .highlight_style(view::selection()),
            list,
            &mut picker.selection,
        );
    }
    frame.render_widget(
        widgets::Paragraph::new(view::clean(hint))
            .wrap(widgets::Wrap { trim: false })
            .dim(),
        note,
    );
}

fn draw_form(frame: &mut Frame, form: &mut Form, read_only: bool, area: Rect) {
    if form.helping {
        let field = form.visible().get(form.focus).map(|&i| &form.fields[i]);
        let text = format!(
            "{}\n\n{}\n\n{}\n\n{}",
            form.context,
            field.map_or_else(
                || "Review all values before saving.".into(),
                |f| format!("{}: {}\n{}", f.label, f.text_value(), f.hint)
            ),
            "Tab/Shift-Tab: field\nEnter: choose/advance; use the named action to review\nEsc: back; changed drafts ask before discarding\nCtrl-U: clear text\nPasted text never submits.\nCtrl-C: quit immediately (draft discarded)",
            form.error.as_deref().unwrap_or("")
        );
        detail(
            frame,
            "Form details · Esc back",
            &text,
            &mut form.scroll,
            area,
        );
        return;
    }
    if form.discarding {
        frame.render_widget(widgets::Paragraph::new("Discard this draft?\n\nYour changes have not been saved.\n\nEnter / Esc: keep editing\nd: discard draft").wrap(widgets::Wrap { trim: false }), area);
        return;
    }
    if let Some(picker) = &mut form.picker {
        choose(
            frame,
            picker,
            area,
            |(_, label)| label.clone(),
            "Enter: choose · Esc: keep previous value",
        );
        return;
    }
    if let Some(review) = &form.review {
        let [body, error, action] = layout::Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(u16::from(form.error.is_some()) * 2),
            Constraint::Length(1),
        ])
        .areas(area);
        detail(
            frame,
            &format!("Review · {}", form.title()),
            &review.summary,
            &mut form.scroll,
            body,
        );
        if let Some(message) = &form.error {
            frame.render_widget(
                widgets::Paragraph::new(view::clean(message))
                    .red()
                    .wrap(widgets::Wrap { trim: false }),
                error,
            );
        }
        let mut label = format!("Enter: {}", form.action());
        if read_only {
            label = "Waiting for database…".into();
        }
        frame.render_widget(
            widgets::Paragraph::new(view::truncate(&label, usize::from(action.width)))
                .style(view::selection()),
            action,
        );
        return;
    }
    let visible = form.visible();
    form.focus = form.focus.min(visible.len());
    let [title, hint, content, action] = layout::Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(2),
        Constraint::Length(1),
    ])
    .areas(area);
    let mut heading = form.title().to_owned();
    if matches!(form.kind, super::form::Kind::Project) {
        heading.push_str(&format!(" · step {}/2", form.step + 1));
    }
    frame.render_widget(
        widgets::Paragraph::new(view::truncate(&heading, usize::from(title.width))).bold(),
        title,
    );
    let hint_text = visible
        .get(form.focus)
        .map_or("Enter: review before saving", |&i| form.fields[i].hint);
    frame.render_widget(
        widgets::Paragraph::new(view::truncate(hint_text, usize::from(hint.width))).dim(),
        hint,
    );
    let error_height = u16::from(form.error.is_some()) * 2;
    let slots = usize::from(content.height.saturating_sub(error_height) / 2).max(1);
    let first = form
        .focus
        .min(visible.len().saturating_sub(1))
        .saturating_sub(slots - 1);
    let mut y = content.y;
    let retainer = form.choice(FieldId::Pricing) == Some(Choice::Retainer);
    for (position, &i) in visible.iter().enumerate().skip(first).take(slots) {
        let field = &form.fields[i];
        let focused = position == form.focus;
        let rect = Rect::new(
            content.x,
            y,
            content.width,
            2.min(content.bottom().saturating_sub(y)),
        );
        let mut label = field.label.to_owned();
        if field.id == FieldId::Rate && retainer {
            label = "Overage rate · optional".into();
        }
        label.push_str(&format!(" · {}/{}", position + 1, visible.len()));
        match &field.value {
            Value::Text(input) => view::input_field(frame, &label, input, "", rect, focused),
            Value::Choice { .. } => {
                let mut heading = text::Line::from(label).dim();
                if focused {
                    heading = text::Line::from(format!("› {}", field.label)).bold();
                }
                let mut value = field.text_value();
                if value.is_empty() {
                    value = "Enter to choose…";
                }
                frame.render_widget(
                    widgets::Paragraph::new(vec![
                        heading,
                        text::Line::from(view::truncate(value, usize::from(rect.width))),
                    ]),
                    rect,
                );
            }
        }
        y += rect.height;
        if focused && let Some(message) = &form.error {
            let rect = Rect::new(
                content.x,
                y,
                content.width,
                error_height.min(content.bottom().saturating_sub(y)),
            );
            frame.render_widget(
                widgets::Paragraph::new(format!("Error: {}", view::clean(message)))
                    .red()
                    .wrap(widgets::Wrap { trim: false }),
                rect,
            );
            y += rect.height;
        }
    }
    let mut label = "Review";
    if matches!(form.kind, super::form::Kind::Project) && form.step == 0 {
        label = "Continue to agreement";
    }
    let mut line = text::Line::from(format!("[ {label} ]"));
    if form.focus == visible.len() {
        line = line.style(view::selection());
    }
    frame.render_widget(widgets::Paragraph::new(line), action);
}

/// Wrapped, bounded scrolling keeps every value accessible without horizontal scrolling.
fn detail(frame: &mut Frame, title: &str, text: &str, scroll: &mut u16, area: Rect) {
    let [heading, body] =
        layout::Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    let paragraph =
        widgets::Paragraph::new(view::clean_lines(text)).wrap(widgets::Wrap { trim: false });
    let lines = paragraph.line_count(body.width);
    let maximum = u16::try_from(lines.saturating_sub(usize::from(body.height))).unwrap_or(u16::MAX);
    *scroll = (*scroll).min(maximum);
    let label = format!("{} · {}/{}", title, usize::from(*scroll) + 1, lines.max(1));
    frame.render_widget(
        widgets::Paragraph::new(view::truncate(&label, usize::from(heading.width))).bold(),
        heading,
    );
    frame.render_widget(paragraph.scroll((*scroll, 0)), body);
}

pub(super) fn shortcuts(app: &App, compact: bool) -> Option<&'static str> {
    match &app.workspace.overlay {
        Some(Overlay::Form(form)) if form.helping => Some("↑↓ scroll  Esc back"),
        Some(Overlay::Form(form)) if form.discarding => Some("Enter keep  d discard"),
        Some(Overlay::Form(form)) if form.picker.is_some() => {
            Some("↑↓ choose  Enter set  Esc back")
        }
        Some(Overlay::Form(form))
            if form.review.is_some()
                && matches!(
                    form.kind,
                    super::form::Kind::Cancel(_) | super::form::Kind::DeleteEntry(_)
                ) =>
        {
            Some("↑↓ scroll  Esc cancel  F1 help")
        }
        Some(Overlay::Form(form)) if form.review.is_some() => {
            Some("↑↓ scroll  Esc edit  F1 details")
        }
        Some(Overlay::Form(_)) if compact => Some("Tab field  Enter next  F1 help"),
        Some(Overlay::Form(_)) => {
            Some("Tab / Shift-Tab field   Enter choose/next   Esc back   F1 details")
        }
        Some(Overlay::Entries(_)) => Some("Search  Enter review  Tab info"),
        Some(Overlay::Detail { .. }) => Some("↑↓ scroll  : actions  Esc back"),
        Some(_) => Some("↑↓ choose  Enter open  Esc back"),
        None if app.workspace.section == Section::Agreements
            && app.workspace.browser().is_some_and(|b| b.preview) =>
        {
            Some("↑↓ scroll  l log  Tab list  : menu")
        }
        None if app.workspace.browser().is_some_and(|b| b.preview) => {
            Some("↑↓ scroll  Tab list  : actions")
        }
        None if app.workspace.browser().is_some_and(|b| b.searching) => {
            Some("↑↓ select  Enter done  Esc done")
        }
        None if app.workspace.section == Section::Agreements
            && app.workspace.catalogs.projects.is_empty() =>
        {
            Some("n setup  : actions  ? help")
        }
        None if app.workspace.section == Section::Agreements && compact => {
            Some("l log time  / search  : menu")
        }
        None if app.workspace.section == Section::Agreements => Some(
            "↑↓/jk select   l log time   n replace terms   Enter details   / search   : actions",
        ),
        None if app.workspace.section == Section::Settings => {
            Some("↑↓ scroll  Enter change  : menu")
        }
        None if app.workspace.section != Section::Dashboard && compact => {
            Some("n new  / search  : menu  ? help")
        }
        None if app.workspace.section != Section::Dashboard => {
            Some("↑↓/jk select   n new   Enter open   / search   : actions   ? help   q quit")
        }
        _ => None,
    }
}
