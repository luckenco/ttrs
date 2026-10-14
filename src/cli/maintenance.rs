use std::{fs, path::Path};

use tt::{maintenance, portable};

use super::{
    Command, Error,
    app::{core_error, database_path, line, now},
    backend, io_error,
};

// Runs before Tracker or timezone configuration is opened. File-only verification
// and destructive confirmation must not resolve or touch the database path.
pub(super) fn run(
    command: &Command,
    explicit: Option<backend::Selection>,
) -> Option<Result<(), Error>> {
    // Verification and confirmation are deliberately before even saved backend parsing.
    if let Command::BackupVerify { file } = command {
        return Some(verify(file).and_then(|snapshot| {
            line(format_args!("Verified portable backup {}", file.display()))?;
            summary(&snapshot)
        }));
    }
    if matches!(command, Command::RestoreActivate { yes: false, .. }) {
        return Some(Err(Error::usage(
            "Reviewed-loss activation requires --yes and the exact --report file.",
        )));
    }
    if matches!(command, Command::RestoreReplace { yes: false, .. }) {
        return Some(Err(Error::usage(
            "Replacement discards work absent from the backup. Close all database handles and rerun with --yes to confirm. This is low-level restore, not reviewed-loss activation.",
        )));
    }
    if !matches!(
        command,
        Command::BackupCreate { .. }
            | Command::BackupExport { .. }
            | Command::RestoreImport { .. }
            | Command::RestoreReplace { .. }
            | Command::WorkspaceCreate
            | Command::BackupDownload { .. }
            | Command::RestoreStage { .. }
            | Command::RestoreFreeze { .. }
            | Command::RestoreReport { .. }
            | Command::RestoreActivate { .. }
            | Command::RestoreStatus { .. }
            | Command::RestoreAbort { .. }
    ) {
        return None;
    }
    Some(selected(command, backend::selection(explicit)))
}

fn selected(command: &Command, selection: Result<backend::Selection, Error>) -> Result<(), Error> {
    if selection? == backend::Selection::Remote {
        return remote(command);
    }
    match command {
        Command::BackupCreate { output } => create(
            output
                .as_deref()
                .ok_or_else(|| Error::usage("local backup create requires --output"))?,
        ),
        Command::BackupExport { output } => export(output),
        Command::RestoreImport { file } => import(file),
        Command::RestoreReplace { file, yes: true } => replace(file),
        _ => Err(Error::usage(
            "this maintenance operation requires the remote backend",
        )),
    }
}

fn remote(command: &Command) -> Result<(), Error> {
    if matches!(
        command,
        Command::RestoreImport { .. } | Command::RestoreReplace { .. }
    ) {
        return Err(Error::usage(
            "native low-level restore is unavailable remotely; use stage, freeze, report, then activate --report FILE --yes",
        ));
    }
    if matches!(
        command,
        Command::BackupCreate {
            output: Some(_),
            ..
        }
    ) {
        return Err(Error::usage(
            "backup create --output is not supported on the remote backend; remote create publishes a manifest, or use backup export --output FILE to download",
        ));
    }
    let admin = tt::client::Maintenance::new(backend::remote_config()?).map_err(core_error)?;
    let value = match command {
        Command::WorkspaceCreate => admin.bootstrap(),
        Command::BackupCreate { .. } => admin.backup(),
        Command::BackupExport { output } => {
            let published = admin.backup().map_err(core_error)?;
            let manifest = published["key"]
                .as_str()
                .ok_or_else(|| Error::runtime("invalid backup pointer"))?;
            let snapshot = admin.download(manifest, output).map_err(core_error)?;
            return summary(&snapshot);
        }
        Command::BackupDownload { manifest, output } => {
            let snapshot = admin.download(manifest, output).map_err(core_error)?;
            return summary(&snapshot);
        }
        Command::RestoreStage { file } => admin.stage(&verify_remote(file)?),
        Command::RestoreFreeze { job } => admin.freeze(job),
        Command::RestoreReport { job, output } => {
            admin.report(job, output).map_err(core_error)?;
            return line(format_args!(
                "Saved immutable report {}. Review before activate --report FILE --yes.",
                output.display()
            ));
        }
        Command::RestoreActivate { job, report, yes } => admin.activate(job, report, *yes),
        Command::RestoreStatus { job } => admin
            .status(job.as_deref())
            .map(|v| v.unwrap_or(serde_json::Value::Null)),
        Command::RestoreAbort { job } => admin.abort(job),
        _ => unreachable!("selected maintenance command"),
    }
    .map_err(core_error)?;
    line(format_args!(
        "{}",
        serde_json::to_string(&value).map_err(core_error)?
    ))
}

fn create(output: &Path) -> Result<(), Error> {
    maintenance::backup(database_path()?, output).map_err(core_error)?;
    line(format_args!(
        "Created standalone SQLite backup {}",
        output.display()
    ))
}

fn export(output: &Path) -> Result<(), Error> {
    let snapshot = maintenance::export(database_path()?, now()?).map_err(core_error)?;
    maintenance::save(&snapshot, output).map_err(core_error)?;
    line(format_args!(
        "Exported portable backup {}",
        output.display()
    ))?;
    summary(&snapshot)
}

fn verify(file: &Path) -> Result<portable::Verified, Error> {
    portable::verify(&fs::read(file).map_err(io_error)?).map_err(core_error)
}

fn verify_remote(file: &Path) -> Result<portable::Verified, Error> {
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(file)
        .map_err(io_error)?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(Error::runtime("portable input exceeds 2 MiB budget"));
    }
    portable::verify(&bytes).map_err(core_error)
}

fn import(file: &Path) -> Result<(), Error> {
    let snapshot = verify(file)?;
    let path = database_path()?;
    if path.try_exists().map_err(io_error)? {
        return Err(Error::runtime(
            "Import requires a nonexistent database; target already exists.",
        ));
    }
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    maintenance::import(&path, &snapshot).map_err(core_error)?;
    line(format_args!(
        "Imported into {} with a new workspace identity.",
        path.display()
    ))?;
    summary(&snapshot)?;
    line(format_args!(
        "Running timers remain running with their original start times."
    ))
}

fn replace(file: &Path) -> Result<(), Error> {
    let snapshot = verify(file)?;
    let path = database_path()?;
    maintenance::restore(&path, &snapshot).map_err(core_error)?;
    line(format_args!(
        "Replaced {}. Final safety copy saved alongside the database (pre-restore). Workspace identity preserved; incarnation renewed.",
        path.display()
    ))?;
    summary(&snapshot)?;
    line(format_args!(
        "Running timers remain running with their original start times."
    ))
}

fn summary(snapshot: &portable::Verified) -> Result<(), Error> {
    let manifest = snapshot.manifest();
    line(format_args!(
        "Source workspace: {}; incarnation: {}; revision: {}",
        manifest.workspace, manifest.incarnation, manifest.revision
    ))?;
    line(format_args!(
        "Rows: clients={}, projects={}, agreements={}, running timers={}, entries={}",
        manifest.counts[0],
        manifest.counts[1],
        manifest.counts[2],
        manifest.counts[3],
        manifest.counts[4]
    ))?;
    line(format_args!(
        "Integrity verification does not authenticate the source; restore only trusted backups."
    ))
}
