use std::{
    collections::HashMap,
    env, fmt, fs,
    io::{self, IsTerminal, Write},
    path::PathBuf,
};

use jiff::{Timestamp, civil, tz::TimeZone};

use tt::{Tracker, command, model};

use super::{Command, Error, TrackingTarget, io_error, terms};

pub(super) fn run(command: Command) -> Result<(), Error> {
    if let Command::ConfigSetTimezone { timezone } = command {
        return set_timezone(&timezone);
    }
    if command == Command::ConfigGetTimezone {
        let zone = timezone()?;
        return line(format_args!("{}", timezone_name(&zone)?));
    }

    let zone = timezone()?;
    let path = database_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    let mut tracker = Tracker::open(path).map_err(core_error)?;

    match command {
        Command::Bare => interactive(&mut tracker, &zone),
        Command::Start { target, scope, at } => {
            let target = resolve_target(&tracker, target)?;
            let started_at = parse_time(at.as_deref().unwrap_or("now"), &zone, now()?)?;
            let timer = tracker
                .start(command::Start::new(target.context, scope, started_at).map_err(core_error)?)
                .map_err(core_error)?;
            line(format_args!(
                "Started {} for {} at {} (ID {})",
                timer.scope(),
                target.label,
                clock(timer.started_at(), &zone),
                timer.id()
            ))
        }
        Command::Stop { id, at } => {
            let id = id
                .parse::<model::TimerId>()
                .map_err(|error| Error::usage(error.to_string()))?;
            let ended_at = parse_time(at.as_deref().unwrap_or("now"), &zone, now()?)?;
            let entry = tracker
                .end(command::End::new(id, ended_at))
                .map_err(core_error)?;
            line(format_args!(
                "Stopped {} at {}",
                entry.scope(),
                clock(entry.ended_at(), &zone)
            ))
        }
        Command::Status => status(&tracker, &zone, now()?),
        Command::Log {
            target,
            scope,
            from,
            to,
        } => {
            let target = resolve_target(&tracker, target)?;
            let reference = now()?;
            let started_at = parse_time(&from, &zone, reference)?;
            let ended_at = parse_time(&to, &zone, reference)?;
            let entry = tracker
                .log(
                    command::LogEntry::new(target.context, scope, started_at, ended_at)
                        .map_err(core_error)?,
                )
                .map_err(core_error)?;
            line(format_args!(
                "Logged {} for {} from {} to {}",
                entry.scope(),
                target.label,
                clock(entry.started_at(), &zone),
                clock(entry.ended_at(), &zone)
            ))
        }
        Command::ClientAdd { name } => {
            let client = tracker.add_client(name).map_err(core_error)?;
            line(format_args!("Added client {}", client.name()))
        }
        Command::ClientList => {
            let clients = tracker.clients().map_err(core_error)?;
            if clients.is_empty() {
                return line(format_args!("No clients."));
            }
            line(format_args!("ID\tNAME"))?;
            for client in clients {
                line(format_args!("{}\t{}", client.id(), client.name()))?;
            }
            Ok(())
        }
        Command::AgreementAdd {
            client,
            name,
            rate,
            currency,
            minimum_hours,
            maximum_hours,
            timezone,
        } => {
            let client = tracker.client_by_name(&client).map_err(core_error)?;
            let (rate, monthly_hours) = terms::parse(
                &rate,
                currency,
                minimum_hours.as_deref(),
                maximum_hours.as_deref(),
            )?;
            let agreement = tracker
                .add_service_agreement(client.id(), name, rate, monthly_hours, timezone)
                .map_err(core_error)?;
            line(format_args!("Added service agreement {}", agreement.name()))
        }
        Command::AgreementList { client } => agreement_list(&tracker, client.as_deref()),
        Command::AgreementArchive { client, name } => {
            let client = tracker.client_by_name(&client).map_err(core_error)?;
            let agreement = tracker
                .service_agreement_by_name(client.id(), &name)
                .map_err(core_error)?;
            tracker
                .archive_service_agreement(agreement.id())
                .map_err(core_error)?;
            line(format_args!(
                "Archived service agreement {}",
                agreement.name()
            ))
        }
        Command::ProjectAdd {
            name,
            client,
            agreement,
        } => {
            let client = tracker.client_by_name(&client).map_err(core_error)?;
            let agreement = tracker
                .service_agreement_by_name(client.id(), &agreement)
                .map_err(core_error)?;
            let project = tracker
                .add_project(name, Some(client.id()), Some(agreement.id()))
                .map_err(core_error)?;
            line(format_args!("Added project {}", project.name()))
        }
        Command::ProjectEdit { name, agreement } => {
            let project = tracker.project_by_name(&name).map_err(core_error)?;
            let client = project.client().ok_or_else(|| {
                Error::runtime(format!("project '{}' has no client", project.name()))
            })?;
            let agreement = tracker
                .service_agreement_by_name(client, &agreement)
                .map_err(core_error)?;
            tracker
                .set_project_agreement(project.id(), agreement.id())
                .map_err(core_error)?;
            line(format_args!("Updated project {}", project.name()))
        }
        Command::ProjectList => project_list(&tracker),
        Command::ConfigSetTimezone { .. } | Command::ConfigGetTimezone => unreachable!(),
    }
}

fn status(tracker: &Tracker, zone: &TimeZone, now: Timestamp) -> Result<(), Error> {
    let timers = tracker.running().map_err(core_error)?;
    if timers.is_empty() {
        return line(format_args!("No running timers."));
    }
    let projects = tracker
        .projects()
        .map_err(core_error)?
        .into_iter()
        .map(|project| (project.id(), project.name().to_owned()))
        .collect::<HashMap<_, _>>();
    let agreements = tracker
        .service_agreements()
        .map_err(core_error)?
        .into_iter()
        .map(|agreement| (agreement.id(), agreement.name().to_owned()))
        .collect::<HashMap<_, _>>();

    line(format_args!(
        "ID\tAGREEMENT\tPROJECT\tSCOPE\tSTARTED\tELAPSED"
    ))?;
    for timer in timers {
        let (agreement, project) = context_names(timer.context(), &agreements, &projects);
        line(format_args!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            timer.id(),
            agreement,
            project,
            timer.scope(),
            clock(timer.started_at(), zone),
            elapsed(timer.started_at(), now)
        ))?;
    }
    Ok(())
}

fn project_list(tracker: &Tracker) -> Result<(), Error> {
    let clients = tracker
        .clients()
        .map_err(core_error)?
        .into_iter()
        .map(|client| (client.id(), client.name().to_owned()))
        .collect::<HashMap<_, _>>();
    let projects = tracker.projects().map_err(core_error)?;
    if projects.is_empty() {
        return line(format_args!("No projects."));
    }

    let agreements = tracker
        .service_agreements()
        .map_err(core_error)?
        .into_iter()
        .map(|agreement| (agreement.id(), agreement.name().to_owned()))
        .collect::<HashMap<_, _>>();
    line(format_args!("ID\tNAME\tCLIENT\tAGREEMENT"))?;
    for project in projects {
        let client = project
            .client()
            .map(|id| {
                clients
                    .get(&id)
                    .expect("projects always reference a client")
                    .as_str()
            })
            .unwrap_or("-");
        let agreement = project
            .active_agreement()
            .and_then(|id| agreements.get(&id))
            .map(String::as_str)
            .unwrap_or("-");
        line(format_args!(
            "{}\t{}\t{}\t{}",
            project.id(),
            project.name(),
            client,
            agreement
        ))?;
    }
    Ok(())
}

fn agreement_list(tracker: &Tracker, client_name: Option<&str>) -> Result<(), Error> {
    let selected_client = client_name
        .map(|name| tracker.client_by_name(name).map(|client| client.id()))
        .transpose()
        .map_err(core_error)?;
    let clients = tracker
        .clients()
        .map_err(core_error)?
        .into_iter()
        .map(|client| (client.id(), client.name().to_owned()))
        .collect::<HashMap<_, _>>();
    let agreements = tracker
        .service_agreements()
        .map_err(core_error)?
        .into_iter()
        .filter(|agreement| selected_client.is_none_or(|client| agreement.client() == client))
        .collect::<Vec<_>>();
    if agreements.is_empty() {
        return line(format_args!("No service agreements."));
    }

    line(format_args!(
        "ID\tCLIENT\tNAME\tRATE\tCURRENCY\tMIN\tMAX\tTIMEZONE\tACTIVE"
    ))?;
    for agreement in agreements {
        let client = clients
            .get(&agreement.client())
            .expect("agreements always reference a client");
        line(format_args!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            agreement.id(),
            client,
            agreement.name(),
            terms::format_rate(agreement.hourly_rate()),
            agreement.hourly_rate().currency(),
            terms::format_minutes(agreement.monthly_hours().minimum_minutes()),
            terms::format_minutes(agreement.monthly_hours().maximum_minutes()),
            agreement
                .billing_timezone()
                .iana_name()
                .expect("billing timezones always have an IANA name"),
            if agreement.is_active() { "yes" } else { "no" }
        ))?;
    }
    Ok(())
}

struct ResolvedTarget {
    context: model::WorkContext,
    label: String,
}

fn resolve_target(tracker: &Tracker, target: TrackingTarget) -> Result<ResolvedTarget, Error> {
    match target {
        TrackingTarget::Project(name) => {
            let project = tracker.project_by_name(&name).map_err(core_error)?;
            let agreement = project
                .active_agreement()
                .ok_or_else(|| core_error(tt::Error::ProjectAgreementMissing(project.id())))?;
            Ok(ResolvedTarget {
                context: model::WorkContext::new(agreement, Some(project.id())),
                label: project.name().to_owned(),
            })
        }
        TrackingTarget::Agreement { client, agreement } => {
            let client = tracker.client_by_name(&client).map_err(core_error)?;
            let agreement = tracker
                .service_agreement_by_name(client.id(), &agreement)
                .map_err(core_error)?;
            Ok(ResolvedTarget {
                context: model::WorkContext::new(agreement.id(), None),
                label: agreement.name().to_owned(),
            })
        }
    }
}

fn context_names<'a>(
    context: model::WorkContext,
    agreements: &'a HashMap<model::ServiceAgreementId, String>,
    projects: &'a HashMap<model::ProjectId, String>,
) -> (&'a str, &'a str) {
    let agreement = agreements
        .get(&context.agreement())
        .expect("recorded work always references an agreement");
    let project = context
        .project()
        .and_then(|project| projects.get(&project))
        .map(String::as_str)
        .unwrap_or("-");
    (agreement, project)
}

fn interactive(tracker: &mut Tracker, zone: &TimeZone) -> Result<(), Error> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(Error::usage(
            "interactive mode requires a terminal; use an explicit command",
        ));
    }

    let timers = tracker.running().map_err(core_error)?;
    if timers.is_empty() {
        interactive_start(tracker, zone)
    } else {
        interactive_stop(tracker, zone, &timers)
    }
}

fn interactive_start(tracker: &mut Tracker, zone: &TimeZone) -> Result<(), Error> {
    let projects = tracker.projects().map_err(core_error)?;
    if projects.is_empty() {
        return Err(Error::runtime(
            "no projects exist; add one with 'tt project add'",
        ));
    }
    let clients = tracker
        .clients()
        .map_err(core_error)?
        .into_iter()
        .map(|client| (client.id(), client.name().to_owned()))
        .collect::<HashMap<_, _>>();

    line(format_args!("Project:"))?;
    for (index, project) in projects.iter().enumerate() {
        match project.client().and_then(|id| clients.get(&id)) {
            Some(client) => line(format_args!("{}. {} ({client})", index + 1, project.name()))?,
            None => line(format_args!("{}. {}", index + 1, project.name()))?,
        }
    }
    let Some(index) = selection(projects.len())? else {
        return cancelled();
    };
    let project = &projects[index];
    let agreement = project
        .active_agreement()
        .ok_or_else(|| core_error(tt::Error::ProjectAgreementMissing(project.id())))?;
    let context = model::WorkContext::new(agreement, Some(project.id()));
    let scopes = tracker.recent_scopes(context, 3).map_err(core_error)?;

    line(format_args!("\nScope:"))?;
    for (index, scope) in scopes.iter().enumerate() {
        line(format_args!("{}. {scope}", index + 1))?;
    }
    line(format_args!("n. New scope"))?;
    let Some(choice) = prompt("> ")? else {
        return cancelled();
    };
    if choice.eq_ignore_ascii_case("q") {
        return cancelled();
    }
    let scope = if choice.eq_ignore_ascii_case("n") {
        let Some(scope) = prompt("New scope: ")? else {
            return cancelled();
        };
        scope
    } else {
        let index = parse_selection(&choice, scopes.len())?;
        scopes[index].clone()
    };

    let Some(at) = prompt("\nStart time [now]: ")? else {
        return cancelled();
    };
    let reference = now()?;
    let started_at = if at.is_empty() {
        reference
    } else {
        parse_time(&at, zone, reference)?
    };
    let timer = tracker
        .start(command::Start::new(context, scope, started_at).map_err(core_error)?)
        .map_err(core_error)?;
    line(format_args!(
        "\nStarted {} for {} at {}",
        timer.scope(),
        project.name(),
        clock(timer.started_at(), zone)
    ))
}

fn interactive_stop(
    tracker: &mut Tracker,
    zone: &TimeZone,
    timers: &[model::RunningTimer],
) -> Result<(), Error> {
    let projects = tracker
        .projects()
        .map_err(core_error)?
        .into_iter()
        .map(|project| (project.id(), project.name().to_owned()))
        .collect::<HashMap<_, _>>();
    let agreements = tracker
        .service_agreements()
        .map_err(core_error)?
        .into_iter()
        .map(|agreement| (agreement.id(), agreement.name().to_owned()))
        .collect::<HashMap<_, _>>();
    let displayed_at = now()?;
    line(format_args!("Running:"))?;
    for (index, timer) in timers.iter().enumerate() {
        let (agreement, project) = context_names(timer.context(), &agreements, &projects);
        line(format_args!(
            "{}. {}\t{}\t{}\t{}",
            index + 1,
            timer.scope(),
            agreement,
            project,
            elapsed(timer.started_at(), displayed_at)
        ))?;
    }
    let Some(index) = selection(timers.len())? else {
        return cancelled();
    };
    let entry = tracker
        .end(command::End::new(timers[index].id(), now()?))
        .map_err(core_error)?;
    line(format_args!(
        "\nStopped {} at {}",
        entry.scope(),
        clock(entry.ended_at(), zone)
    ))
}

fn selection(count: usize) -> Result<Option<usize>, Error> {
    let Some(value) = prompt("> ")? else {
        return Ok(None);
    };
    if value.eq_ignore_ascii_case("q") {
        return Ok(None);
    }
    parse_selection(&value, count).map(Some)
}

fn parse_selection(value: &str, count: usize) -> Result<usize, Error> {
    let selected = value
        .parse::<usize>()
        .map_err(|_| Error::usage("selection must be a listed number or 'q'"))?;
    if selected == 0 || selected > count {
        return Err(Error::usage("selection is out of range"));
    }
    Ok(selected - 1)
}

fn prompt(label: &str) -> Result<Option<String>, Error> {
    let mut stdout = io::stdout().lock();
    write!(stdout, "{label}").map_err(io_error)?;
    stdout.flush().map_err(io_error)?;
    let mut value = String::new();
    if io::stdin().read_line(&mut value).map_err(io_error)? == 0 {
        return Ok(None);
    }
    Ok(Some(value.trim().to_owned()))
}

fn cancelled() -> Result<(), Error> {
    line(format_args!("\nCancelled."))
}

fn parse_time(value: &str, zone: &TimeZone, now: Timestamp) -> Result<Timestamp, Error> {
    if value == "now" {
        return Ok(now);
    }
    if let Ok(timestamp) = value.parse::<Timestamp>() {
        return Ok(timestamp);
    }

    let datetime = if let Some((hour, minute)) = compact_time(value) {
        let date = now.to_zoned(zone.clone()).date();
        date.at(hour, minute, 0, 0)
    } else {
        value
            .parse::<civil::DateTime>()
            .map_err(|_| Error::usage(format!("invalid time '{value}'")))?
    };
    zone.to_ambiguous_timestamp(datetime)
        .unambiguous()
        .map_err(|_| {
            Error::usage(format!(
                "local time '{value}' is ambiguous or does not exist"
            ))
        })
}

fn compact_time(value: &str) -> Option<(i8, i8)> {
    let (hour, minute) = match value.as_bytes() {
        [h1, h2, m1, m2] if value.bytes().all(|byte| byte.is_ascii_digit()) => {
            (digits(*h1, *h2), digits(*m1, *m2))
        }
        [h1, h2, b':', m1, m2]
            if [*h1, *h2, *m1, *m2]
                .into_iter()
                .all(|byte| byte.is_ascii_digit()) =>
        {
            (digits(*h1, *h2), digits(*m1, *m2))
        }
        _ => return None,
    };
    (hour <= 23 && minute <= 59).then_some((hour as i8, minute as i8))
}

fn digits(high: u8, low: u8) -> u8 {
    (high - b'0') * 10 + low - b'0'
}

fn clock(timestamp: Timestamp, zone: &TimeZone) -> String {
    timestamp
        .to_zoned(zone.clone())
        .strftime("%H:%M")
        .to_string()
}

fn elapsed(started_at: Timestamp, now: Timestamp) -> String {
    let seconds = now.duration_since(started_at).as_secs().max(0);
    format!("{:02}:{:02}", seconds / 3600, seconds % 3600 / 60)
}

fn now() -> Result<Timestamp, Error> {
    match env::var("TT_NOW") {
        Ok(value) => value
            .parse()
            .map_err(|_| Error::runtime("TT_NOW must be an explicit timestamp")),
        Err(env::VarError::NotPresent) => Ok(Timestamp::now()),
        Err(error) => Err(Error::runtime(error.to_string())),
    }
}

fn timezone() -> Result<TimeZone, Error> {
    let path = config_path()?;
    match fs::read_to_string(path) {
        Ok(name) => {
            let name = name.trim();
            iana_timezone(name).ok_or_else(|| {
                Error::runtime(format!(
                    "configured timezone '{name}' is not a valid IANA name"
                ))
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(TimeZone::system()),
        Err(error) => Err(io_error(error)),
    }
}

fn set_timezone(name: &str) -> Result<(), Error> {
    iana_timezone(name).ok_or_else(|| Error::usage(format!("invalid IANA timezone '{name}'")))?;
    let path = config_path()?;
    let parent = path.parent().expect("configuration path has a parent");
    fs::create_dir_all(parent).map_err(io_error)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(io_error)?;
    writeln!(temporary, "{name}").map_err(io_error)?;
    temporary
        .persist(path)
        .map_err(|error| io_error(error.error))?;
    line(format_args!("Timezone set to {name}"))
}

fn iana_timezone(name: &str) -> Option<TimeZone> {
    TimeZone::get(name)
        .ok()
        .filter(|zone| zone.iana_name().is_some())
}

fn timezone_name(zone: &TimeZone) -> Result<&str, Error> {
    zone.iana_name().ok_or_else(|| {
        Error::runtime(
            "device timezone has no IANA name; configure one with \
             'tt config set timezone <IANA-timezone>'",
        )
    })
}

fn database_path() -> Result<PathBuf, Error> {
    data_home().map(|path| path.join("tt.sqlite3"))
}

fn config_path() -> Result<PathBuf, Error> {
    config_home().map(|path| path.join("timezone"))
}

fn data_home() -> Result<PathBuf, Error> {
    if let Some(path) = env::var_os("TT_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = env::var_os("XDG_DATA_HOME") {
        return Ok(PathBuf::from(path).join("tt"));
    }
    #[cfg(windows)]
    if let Some(path) = env::var_os("LOCALAPPDATA") {
        return Ok(PathBuf::from(path).join("tt"));
    }
    home().map(|path| path.join(".local/share/tt"))
}

fn config_home() -> Result<PathBuf, Error> {
    if let Some(path) = env::var_os("TT_CONFIG_DIR") {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path).join("tt"));
    }
    #[cfg(windows)]
    if let Some(path) = env::var_os("APPDATA") {
        return Ok(PathBuf::from(path).join("tt"));
    }
    home().map(|path| path.join(".config/tt"))
}

fn home() -> Result<PathBuf, Error> {
    if let Some(path) = env::var_os("HOME") {
        return Ok(PathBuf::from(path));
    }
    #[cfg(windows)]
    if let Some(path) = env::var_os("USERPROFILE") {
        return Ok(PathBuf::from(path));
    }
    Err(Error::runtime("home directory could not be determined"))
}

fn line(arguments: fmt::Arguments<'_>) -> Result<(), Error> {
    writeln!(io::stdout().lock(), "{arguments}").map_err(io_error)
}

fn core_error(error: tt::Error) -> Error {
    Error::runtime(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc() -> TimeZone {
        TimeZone::UTC
    }

    fn now() -> Timestamp {
        "2026-03-12T12:00:00Z".parse().unwrap()
    }

    #[test]
    fn parses_every_documented_time_format() {
        for value in [
            "0900",
            "12:32",
            "2026-03-12 09:00",
            "2026-03-12T09:00:00+01:00",
            "2026-03-12T08:00:00Z",
        ] {
            assert!(parse_time(value, &utc(), now()).is_ok(), "{value}");
        }
    }

    #[test]
    fn rejects_invalid_and_ambiguous_local_times() {
        let berlin = TimeZone::get("Europe/Berlin").unwrap();
        assert!(parse_time("2560", &utc(), now()).is_err());
        assert!(parse_time("2026-03-29 02:30", &berlin, now()).is_err());
        assert!(parse_time("2026-10-25 02:30", &berlin, now()).is_err());
    }

    #[test]
    fn accepts_offsets_during_transitions() {
        let berlin = TimeZone::get("Europe/Berlin").unwrap();
        assert!(parse_time("2026-10-25T02:30:00+02:00", &berlin, now()).is_ok());
    }
}
