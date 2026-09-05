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
        Command::Start { project, scope, at } => {
            let project = resolve_project(&tracker, &project)?;
            let reference = now()?;
            let started_at = parse_time(at.as_deref().unwrap_or("now"), &zone, reference)?;
            let timer = tracker
                .start(command::Start::new(project.id(), scope, started_at).map_err(core_error)?)
                .map_err(core_error)?;
            line(format_args!(
                "Started {} for {} at {} (id:{})",
                timer.scope(),
                project.name(),
                clock(timer.started_at(), &zone),
                timer.id()
            ))?;
            print_limit(&tracker, &timer, reference, &zone)
        }
        Command::Stop { id, at } => {
            let id = id
                .strip_prefix("id:")
                .unwrap_or(&id)
                .parse::<model::TimerId>()
                .map_err(|error| Error::usage(error.to_string()))?;
            let ended_at = parse_time(at.as_deref().unwrap_or("now"), &zone, now()?)?;
            let entry = tracker
                .end(command::End::new(id, ended_at))
                .map_err(core_error)?;
            line(format_args!(
                "Stopped {} at {} (id:{})",
                entry.scope(),
                clock(entry.ended_at(), &zone),
                entry.id()
            ))?;
            if entry.ended_at() < ended_at {
                line(format_args!(
                    "Maximum reached; {} after the cap was not recorded.",
                    duration(entry.ended_at(), ended_at)
                ))?;
            }
            Ok(())
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
                    command::LogEntry::new(target.agreement, scope, started_at, ended_at)
                        .map_err(core_error)?,
                )
                .map_err(core_error)?;
            line(format_args!(
                "Logged {} for {} from {} to {} (id:{})",
                entry.scope(),
                target.label,
                clock(entry.started_at(), &zone),
                clock(entry.ended_at(), &zone),
                entry.id()
            ))
        }
        Command::ClientAdd { name } => {
            let client = tracker.add_client(name).map_err(core_error)?;
            line(format_args!(
                "Added client {} (id:{})",
                client.name(),
                client.id()
            ))
        }
        Command::ClientList => {
            let clients = tracker.clients().map_err(core_error)?;
            if clients.is_empty() {
                return line(format_args!("No clients."));
            }
            line(format_args!("ID\tNAME"))?;
            for client in clients {
                line(format_args!("id:{}\t{}", client.id(), client.name()))?;
            }
            Ok(())
        }
        Command::AgreementAdd {
            project,
            name,
            rate,
            currency,
            minimum_hours,
            maximum_hours,
            effective_on,
        } => {
            let project = resolve_project(&tracker, &project)?;
            let (rate, monthly_hours) = terms::parse(
                &rate,
                currency,
                minimum_hours.as_deref(),
                maximum_hours.as_deref(),
            )?;
            let agreement =
                model::NewAgreement::new(name, rate, monthly_hours, parse_date(&effective_on)?)
                    .map_err(core_error)?;
            let agreement = tracker
                .add_service_agreement(project.id(), agreement, now()?)
                .map_err(core_error)?;
            line(format_args!(
                "Added service agreement {} (id:{})",
                agreement.name(),
                agreement.id()
            ))
        }
        Command::AgreementList { client, all } => agreement_list(&tracker, client.as_deref(), all),
        Command::ProjectAdd {
            name,
            client,
            agreement,
            rate,
            currency,
            minimum_hours,
            maximum_hours,
            timezone,
            effective_on,
        } => {
            let client = resolve_client(&tracker, &client)?;
            let (rate, monthly_hours) = terms::parse(
                &rate,
                currency,
                minimum_hours.as_deref(),
                maximum_hours.as_deref(),
            )?;
            let agreement = model::NewAgreement::new(
                agreement,
                rate,
                monthly_hours,
                parse_date(&effective_on)?,
            )
            .map_err(core_error)?;
            let (project, agreement) = tracker
                .add_project(client.id(), name, timezone, agreement, now()?)
                .map_err(core_error)?;
            line(format_args!(
                "Added project {} (id:{}) with service agreement {} (id:{})",
                project.name(),
                project.id(),
                agreement.name(),
                agreement.id()
            ))
        }
        Command::ProjectArchive { project, on } => {
            let project = resolve_project(&tracker, &project)?;
            let project = tracker
                .archive_project(
                    project.id(),
                    on.as_deref().map(parse_date).transpose()?,
                    now()?,
                )
                .map_err(core_error)?;
            line(format_args!("Archived project {}", project.name()))
        }
        Command::ProjectUnarchive { project } => {
            let project = resolve_project(&tracker, &project)?;
            let project = tracker
                .unarchive_project(project.id())
                .map_err(core_error)?;
            line(format_args!("Unarchived project {}", project.name()))
        }
        Command::ProjectList { all } => project_list(&tracker, all),
        Command::ConfigSetTimezone { .. } | Command::ConfigGetTimezone => unreachable!(),
    }
}

fn status(tracker: &Tracker, zone: &TimeZone, now: Timestamp) -> Result<(), Error> {
    let timers = tracker.running().map_err(core_error)?;
    if timers.is_empty() {
        return line(format_args!("No running timers."));
    }
    line(format_args!(
        "ID\tAGREEMENT\tPROJECT\tSCOPE\tSTARTED\tELAPSED\tCAP LEFT"
    ))?;
    for timer in timers {
        let (agreement, project) = work_names(tracker, timer.agreement())?;
        let limit = tracker.timer_limit(timer.id(), now).map_err(core_error)?;
        line(format_args!(
            "id:{}\t{}\t{}\t{}\t{}\t{}\t{}",
            timer.id(),
            agreement,
            project,
            timer.scope(),
            clock(timer.started_at(), zone),
            elapsed(timer.started_at(), now),
            format_limit(limit)
        ))?;
    }
    Ok(())
}

fn project_list(tracker: &Tracker, all: bool) -> Result<(), Error> {
    let clients = tracker
        .clients()
        .map_err(core_error)?
        .into_iter()
        .map(|client| (client.id(), client.name().to_owned()))
        .collect::<HashMap<_, _>>();
    let projects = tracker
        .projects()
        .map_err(core_error)?
        .into_iter()
        .filter(|project| all || project.is_active())
        .collect::<Vec<_>>();
    if projects.is_empty() {
        return line(format_args!("No projects."));
    }
    let agreements = tracker
        .service_agreements()
        .map_err(core_error)?
        .into_iter()
        .filter(model::ServiceAgreement::is_current)
        .map(|agreement| (agreement.project(), agreement.name().to_owned()))
        .collect::<HashMap<_, _>>();

    line(format_args!("ID\tNAME\tCLIENT\tAGREEMENT\tSTATUS"))?;
    for project in projects {
        let client = clients
            .get(&project.client())
            .expect("projects always reference a client");
        let agreement = agreements
            .get(&project.id())
            .expect("projects always have a current agreement");
        line(format_args!(
            "id:{}\t{}\t{}\t{}\t{}",
            project.id(),
            project.name(),
            client,
            agreement,
            if project.is_active() {
                "active"
            } else {
                "archived"
            }
        ))?;
    }
    Ok(())
}

fn agreement_list(tracker: &Tracker, client_ref: Option<&str>, all: bool) -> Result<(), Error> {
    let selected_client = client_ref
        .map(|reference| resolve_client(tracker, reference).map(|client| client.id()))
        .transpose()?;
    let clients = tracker
        .clients()
        .map_err(core_error)?
        .into_iter()
        .map(|client| (client.id(), client.name().to_owned()))
        .collect::<HashMap<_, _>>();
    let projects = tracker
        .projects()
        .map_err(core_error)?
        .into_iter()
        .map(|project| (project.id(), project))
        .collect::<HashMap<_, _>>();
    let agreements = tracker
        .service_agreements()
        .map_err(core_error)?
        .into_iter()
        .filter(|agreement| {
            let project = projects
                .get(&agreement.project())
                .expect("agreements always reference a project");
            (all || (agreement.is_current() && project.is_active()))
                && selected_client.is_none_or(|client| project.client() == client)
        })
        .collect::<Vec<_>>();
    if agreements.is_empty() {
        return line(format_args!("No service agreements."));
    }

    line(format_args!(
        "ID\tCLIENT\tPROJECT\tNAME\tRATE\tCURRENCY\tMIN\tMAX\tEFFECTIVE\tSTATUS"
    ))?;
    for agreement in agreements {
        let project = projects
            .get(&agreement.project())
            .expect("agreements always reference a project");
        let client = clients
            .get(&project.client())
            .expect("projects always reference a client");
        line(format_args!(
            "id:{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            agreement.id(),
            client,
            project.name(),
            agreement.name(),
            terms::format_rate(agreement.hourly_rate()),
            agreement.hourly_rate().currency(),
            terms::format_hours(agreement.monthly_hours().minimum()),
            terms::format_hours(agreement.monthly_hours().maximum()),
            agreement.effective_on(),
            agreement_status(project, &agreement)
        ))?;
    }
    Ok(())
}

fn agreement_status(project: &model::Project, agreement: &model::ServiceAgreement) -> &'static str {
    if !project.is_active() {
        return "inactive";
    }
    if agreement.is_current() {
        "current"
    } else {
        "replaced"
    }
}

struct ResolvedTarget {
    agreement: model::ServiceAgreementId,
    label: String,
}

fn resolve_target(tracker: &Tracker, target: TrackingTarget) -> Result<ResolvedTarget, Error> {
    match target {
        TrackingTarget::Project(reference) => {
            let project = resolve_project(tracker, &reference)?;
            let agreement = tracker
                .current_service_agreement(project.id())
                .map_err(core_error)?;
            Ok(ResolvedTarget {
                agreement: agreement.id(),
                label: project.name().to_owned(),
            })
        }
        TrackingTarget::Agreement(reference) => {
            let agreement = resolve_agreement(tracker, &reference)?;
            let project = tracker.project(agreement.project()).map_err(core_error)?;
            Ok(ResolvedTarget {
                agreement: agreement.id(),
                label: project.name().to_owned(),
            })
        }
    }
}

fn resolve_client(tracker: &Tracker, reference: &str) -> Result<model::Client, Error> {
    let Some(id) = reference.strip_prefix("id:") else {
        return tracker.client_by_name(reference).map_err(core_error);
    };
    let id = id
        .parse::<model::ClientId>()
        .map_err(|_| Error::usage(format!("invalid client reference '{reference}'")))?;
    tracker.client(id).map_err(core_error)
}

fn resolve_project(tracker: &Tracker, reference: &str) -> Result<model::Project, Error> {
    let Some(id) = reference.strip_prefix("id:") else {
        return tracker.project_by_name(reference).map_err(core_error);
    };
    let id = id
        .parse::<model::ProjectId>()
        .map_err(|_| Error::usage(format!("invalid project reference '{reference}'")))?;
    tracker.project(id).map_err(core_error)
}

fn resolve_agreement(tracker: &Tracker, reference: &str) -> Result<model::ServiceAgreement, Error> {
    let id = reference
        .strip_prefix("id:")
        .ok_or_else(|| Error::usage("agreement references must use id:<number>"))?
        .parse::<model::ServiceAgreementId>()
        .map_err(|_| Error::usage(format!("invalid agreement reference '{reference}'")))?;
    tracker.service_agreement(id).map_err(core_error)
}

fn work_names(
    tracker: &Tracker,
    agreement: model::ServiceAgreementId,
) -> Result<(String, String), Error> {
    let agreement = tracker.service_agreement(agreement).map_err(core_error)?;
    let project = tracker.project(agreement.project()).map_err(core_error)?;
    Ok((agreement.name().to_owned(), project.name().to_owned()))
}

fn print_limit(
    tracker: &Tracker,
    timer: &model::RunningTimer,
    now: Timestamp,
    timezone: &TimeZone,
) -> Result<(), Error> {
    let Some(limit) = tracker.timer_limit(timer.id(), now).map_err(core_error)? else {
        return Ok(());
    };
    match limit.cap_at() {
        Some(cap) => line(format_args!(
            "Cap remaining: {}; timer caps at {}.",
            format_limit(Some(limit)),
            clock(cap, timezone)
        )),
        None => line(format_args!(
            "Cap remaining this month: {}.",
            format_limit(Some(limit))
        )),
    }
}

fn format_limit(limit: Option<model::TimerLimit>) -> String {
    let Some(limit) = limit else {
        return "-".into();
    };
    if limit.is_reached() {
        return "LIMIT REACHED".into();
    }
    let remaining = format_us(limit.remaining_us());
    if limit.remaining_us() <= 3_600_000_000 {
        return format!("WARNING: {remaining}");
    }
    remaining
}

fn format_us(microseconds: i64) -> String {
    let minutes = microseconds.max(0) / 60_000_000;
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

fn duration(start: Timestamp, end: Timestamp) -> String {
    format_us(end.as_microsecond().saturating_sub(start.as_microsecond()))
}

fn parse_date(value: &str) -> Result<civil::Date, Error> {
    value
        .parse()
        .map_err(|_| Error::usage(format!("invalid date '{value}'")))
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
    let projects = tracker
        .projects()
        .map_err(core_error)?
        .into_iter()
        .filter(model::Project::is_active)
        .collect::<Vec<_>>();
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
    let agreements = tracker
        .service_agreements()
        .map_err(core_error)?
        .into_iter()
        .filter(model::ServiceAgreement::is_current)
        .map(|agreement| (agreement.project(), agreement.name().to_owned()))
        .collect::<HashMap<_, _>>();

    line(format_args!("Project:"))?;
    for (index, project) in projects.iter().enumerate() {
        let client = clients
            .get(&project.client())
            .expect("projects always reference a client");
        let agreement = agreements
            .get(&project.id())
            .expect("projects always have a current agreement");
        line(format_args!(
            "{}. {} ({client}; {agreement})",
            index + 1,
            project.name()
        ))?;
    }
    let Some(index) = selection(projects.len())? else {
        return cancelled();
    };
    let project = &projects[index];
    let scopes = tracker.recent_scopes(project.id(), 3).map_err(core_error)?;

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
        .start(command::Start::new(project.id(), scope, started_at).map_err(core_error)?)
        .map_err(core_error)?;
    line(format_args!(
        "\nStarted {} for {} at {} (id:{})",
        timer.scope(),
        project.name(),
        clock(timer.started_at(), zone),
        timer.id()
    ))?;
    print_limit(tracker, &timer, reference, zone)
}

fn interactive_stop(
    tracker: &mut Tracker,
    zone: &TimeZone,
    timers: &[model::RunningTimer],
) -> Result<(), Error> {
    let displayed_at = now()?;
    line(format_args!("Running:"))?;
    for (index, timer) in timers.iter().enumerate() {
        let (agreement, project) = work_names(tracker, timer.agreement())?;
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
    let requested_end = now()?;
    let entry = tracker
        .end(command::End::new(timers[index].id(), requested_end))
        .map_err(core_error)?;
    line(format_args!(
        "\nStopped {} at {} (id:{})",
        entry.scope(),
        clock(entry.ended_at(), zone),
        entry.id()
    ))?;
    if entry.ended_at() < requested_end {
        line(format_args!(
            "Maximum reached; {} after the cap was not recorded.",
            duration(entry.ended_at(), requested_end)
        ))?;
    }
    Ok(())
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
