use std::{
    env, fmt, fs,
    io::{self, Write},
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

    match &command {
        Command::Cancel { yes: false, .. } => {
            return Err(Error::usage(
                "Cancellation records no completed time. Rerun with --yes to confirm.",
            ));
        }
        Command::EntryDelete { yes: false, .. } => {
            return Err(Error::usage(
                "Deletion is permanent and changes estimates and usage. Rerun with --yes to confirm.",
            ));
        }
        _ => {}
    }
    let zone = timezone()?;
    let path = database_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    let mut tracker = Tracker::open(path).map_err(core_error)?;

    match command {
        Command::Bare => super::tui::run(&mut tracker, &zone),
        Command::Start { project, scope, at } => {
            let project = resolve_project(&tracker, &project)?;
            let reference = now()?;
            let started_at = parse_time(at.as_deref().unwrap_or("now"), &zone, reference)?;
            let agreement = tracker
                .current_service_agreement(project.id())
                .map_err(core_error)?;
            let timer = tracker
                .start(command::Start::new(agreement.id(), scope, started_at).map_err(core_error)?)
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
        Command::Cancel { id, .. } => {
            let id = id
                .strip_prefix("id:")
                .unwrap_or(&id)
                .parse::<model::TimerId>()
                .map_err(|error| Error::usage(error.to_string()))?;
            let timer = tracker.timer(id).map_err(core_error)?;
            tracker.cancel_if_current(&timer).map_err(core_error)?;
            line(format_args!(
                "Cancelled {} (id:{}); no entry recorded.",
                timer.scope(),
                id
            ))
        }
        Command::EntryList { agreement } => {
            let agreement = resolve_agreement(&tracker, &agreement)?;
            entry_list(&tracker, agreement.id(), &zone)
        }
        Command::EntryDelete { id, .. } => {
            let id = id
                .strip_prefix("id:")
                .unwrap_or(&id)
                .parse::<model::EntryId>()
                .map_err(|error| Error::usage(error.to_string()))?;
            let entry = tracker.entry(id).map_err(core_error)?;
            tracker
                .delete_entry_if_current(&entry)
                .map_err(core_error)?;
            line(format_args!("Deleted {} (id:{}).", entry.scope(), id))
        }
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
            fee,
            included_hours,
            currency,
            minimum_hours,
            maximum_hours,
            effective_on,
        } => {
            let project = resolve_project(&tracker, &project)?;
            let (pricing, monthly_hours) = terms::parse(
                rate.as_deref(),
                fee.as_deref(),
                included_hours.as_deref(),
                currency,
                minimum_hours.as_deref(),
                maximum_hours.as_deref(),
            )?;
            let agreement =
                model::NewAgreement::new(name, pricing, monthly_hours, parse_date(&effective_on)?)
                    .map_err(core_error)?;
            let previous = tracker
                .current_service_agreement(project.id())
                .map_err(core_error)?;
            let agreement = tracker
                .add_service_agreement(previous.id(), agreement, now()?)
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
            fee,
            included_hours,
            currency,
            minimum_hours,
            maximum_hours,
            timezone,
            effective_on,
        } => {
            let client = resolve_client(&tracker, &client)?;
            let (pricing, monthly_hours) = terms::parse(
                rate.as_deref(),
                fee.as_deref(),
                included_hours.as_deref(),
                currency,
                minimum_hours.as_deref(),
                maximum_hours.as_deref(),
            )?;
            let agreement = model::NewAgreement::new(
                agreement,
                pricing,
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
    let statuses = tracker.status(now).map_err(core_error)?;
    if statuses.is_empty() {
        return line(format_args!("No running timers."));
    }
    line(format_args!(
        "ID\tAGREEMENT\tPROJECT\tSCOPE\tSTARTED\tELAPSED\tCAP LEFT"
    ))?;
    for status in statuses {
        let timer = status.timer;
        line(format_args!(
            "id:{}\t{}\t{}\t{}\t{}\t{}\t{}",
            timer.id(),
            status.agreement.name(),
            status.project.name(),
            timer.scope(),
            clock(timer.started_at(), zone),
            elapsed(timer.started_at(), now),
            format_limit(status.limit)
        ))?;
    }
    Ok(())
}

fn entry_list(
    tracker: &Tracker,
    agreement: model::ServiceAgreementId,
    zone: &TimeZone,
) -> Result<(), Error> {
    let entries = tracker.entries(agreement).map_err(core_error)?;
    if entries.is_empty() {
        return line(format_args!("No completed entries."));
    }
    line(format_args!("ID\tSCOPE\tFROM\tTO\tDURATION"))?;
    for entry in entries {
        line(format_args!(
            "id:{}\t{}\t{}\t{}\t{}",
            entry.id(),
            entry.scope(),
            entry
                .started_at()
                .to_zoned(zone.clone())
                .strftime("%F %T%.f %:z"),
            entry
                .ended_at()
                .to_zoned(zone.clone())
                .strftime("%F %T%.f %:z"),
            duration(entry.started_at(), entry.ended_at())
        ))?;
    }
    Ok(())
}

fn project_list(tracker: &Tracker, all: bool) -> Result<(), Error> {
    let projects = tracker.project_list(all).map_err(core_error)?;
    if projects.is_empty() {
        return line(format_args!("No projects."));
    }
    line(format_args!("ID\tNAME\tCLIENT\tAGREEMENT\tSTATUS"))?;
    for row in projects {
        line(format_args!(
            "id:{}\t{}\t{}\t{}\t{}",
            row.agreement.project(),
            row.project_name,
            row.client_name,
            row.agreement.name(),
            if row.project_active {
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
    let agreements = tracker
        .agreement_list(selected_client, all)
        .map_err(core_error)?;
    if agreements.is_empty() {
        return line(format_args!("No service agreements."));
    }

    line(format_args!(
        "ID\tCLIENT\tPROJECT\tNAME\tRATE\tCURRENCY\tMIN\tMAX\tEFFECTIVE\tSTATUS\tFEE\tINCLUDED"
    ))?;
    for row in agreements {
        let agreement = &row.agreement;
        let (fee, included) = match agreement.pricing() {
            model::Pricing::Hourly(_) => ("-".into(), "-".into()),
            model::Pricing::Retainer(terms) => (
                terms::format_amount(terms.fee().amount_micros()),
                terms.included_hours().to_string(),
            ),
        };
        line(format_args!(
            "id:{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            agreement.id(),
            row.client_name,
            row.project_name,
            agreement.name(),
            agreement.pricing().hourly_rate().map_or_else(
                || "-".into(),
                |rate| terms::format_amount(rate.amount_micros())
            ),
            agreement.pricing().currency(),
            terms::format_hours(agreement.monthly_hours().minimum()),
            terms::format_hours(agreement.monthly_hours().maximum()),
            agreement.effective_on(),
            agreement_status(&row),
            fee,
            included,
        ))?;
    }
    Ok(())
}

fn agreement_status(row: &model::AgreementListing) -> &'static str {
    if !row.project_active {
        return "inactive";
    }
    if row.agreement.is_current() {
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

pub(super) fn format_limit(limit: Option<model::TimerLimit>) -> String {
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

pub(super) fn parse_date(value: &str) -> Result<civil::Date, Error> {
    value
        .parse()
        .map_err(|_| Error::usage(format!("invalid date '{value}'")))
}

pub(super) fn parse_time(value: &str, zone: &TimeZone, now: Timestamp) -> Result<Timestamp, Error> {
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

pub(super) fn clock(timestamp: Timestamp, zone: &TimeZone) -> String {
    timestamp
        .to_zoned(zone.clone())
        .strftime("%H:%M")
        .to_string()
}

fn elapsed(started_at: Timestamp, now: Timestamp) -> String {
    let seconds = now.duration_since(started_at).as_secs().max(0);
    format!("{:02}:{:02}", seconds / 3600, seconds % 3600 / 60)
}

pub(super) fn now() -> Result<Timestamp, Error> {
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
    save_timezone(name)?;
    line(format_args!("Timezone set to {name}"))
}

/// Persists a validated timezone without writing into the active terminal UI.
pub(super) fn save_timezone(name: &str) -> Result<TimeZone, Error> {
    write_timezone(&config_path()?, name)
}

fn write_timezone(path: &std::path::Path, name: &str) -> Result<TimeZone, Error> {
    let zone = iana_timezone(name)
        .ok_or_else(|| Error::usage(format!("invalid IANA timezone '{name}'")))?;
    let parent = path.parent().expect("configuration path has a parent");
    fs::create_dir_all(parent).map_err(io_error)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(io_error)?;
    writeln!(temporary, "{name}").map_err(io_error)?;
    temporary
        .persist(path)
        .map_err(|error| io_error(error.error))?;
    Ok(zone)
}

pub(super) fn timezone_source() -> Result<&'static str, Error> {
    if config_path()?.try_exists().map_err(io_error)? {
        return Ok("Configured");
    }
    Ok("Device timezone")
}

pub(super) fn iana_timezone(name: &str) -> Option<TimeZone> {
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
    fn timezone_persistence_validates_before_replacing_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("timezone");
        assert_eq!(
            write_timezone(&path, "Europe/Berlin").unwrap().iana_name(),
            Some("Europe/Berlin")
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "Europe/Berlin\n");
        assert!(write_timezone(&path, "not/a/timezone").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "Europe/Berlin\n");
        write_timezone(&path, "UTC").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "UTC\n");
    }

    #[test]
    fn accepts_offsets_during_transitions() {
        let berlin = TimeZone::get("Europe/Berlin").unwrap();
        assert!(parse_time("2026-10-25T02:30:00+02:00", &berlin, now()).is_ok());
    }
}
