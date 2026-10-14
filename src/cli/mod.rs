use std::{fmt, io, io::Write, process::ExitCode};

mod app;
mod backend;
pub mod clap;
mod maintenance;
mod terms;
mod tui;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TrackingTarget {
    Project(String),
    Agreement(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    Bare,
    WithBackend {
        backend: backend::Selection,
        command: Box<Command>,
    },
    ConfigBackend {
        backend: backend::Selection,
        url: Option<String>,
        workspace: Option<String>,
        credential_env: Option<String>,
        credential_file: Option<std::path::PathBuf>,
    },
    RequestList,
    WorkspaceCreate,
    BackupDownload {
        manifest: String,
        output: std::path::PathBuf,
    },
    RestoreStage {
        file: std::path::PathBuf,
    },
    RestoreFreeze {
        job: String,
    },
    RestoreReport {
        job: String,
        output: std::path::PathBuf,
    },
    RestoreActivate {
        job: String,
        report: std::path::PathBuf,
        yes: bool,
    },
    RestoreStatus {
        job: Option<String>,
    },
    RestoreAbort {
        job: String,
    },
    RequestStatus {
        request: String,
    },
    RequestRetry {
        request: String,
    },
    BackupCreate {
        output: Option<std::path::PathBuf>,
    },
    BackupExport {
        output: std::path::PathBuf,
    },
    BackupVerify {
        file: std::path::PathBuf,
    },
    RestoreImport {
        file: std::path::PathBuf,
    },
    RestoreReplace {
        file: std::path::PathBuf,
        yes: bool,
    },
    Start {
        project: String,
        scope: String,
        at: Option<String>,
    },
    Stop {
        id: String,
        at: Option<String>,
    },
    Status,
    Cancel {
        id: String,
        yes: bool,
    },
    EntryList {
        agreement: String,
    },
    EntryDelete {
        id: String,
        yes: bool,
    },
    Log {
        target: TrackingTarget,
        scope: String,
        from: String,
        to: String,
    },
    ClientAdd {
        name: String,
    },
    ClientList,
    AgreementAdd {
        project: String,
        name: String,
        rate: Option<String>,
        fee: Option<String>,
        included_hours: Option<String>,
        currency: String,
        minimum_hours: Option<String>,
        maximum_hours: Option<String>,
        effective_on: String,
    },
    AgreementList {
        client: Option<String>,
        all: bool,
    },
    ProjectAdd {
        name: String,
        client: String,
        agreement: String,
        rate: Option<String>,
        fee: Option<String>,
        included_hours: Option<String>,
        currency: String,
        minimum_hours: Option<String>,
        maximum_hours: Option<String>,
        timezone: String,
        effective_on: String,
    },
    ProjectArchive {
        project: String,
        on: Option<String>,
    },
    ProjectUnarchive {
        project: String,
    },
    ProjectList {
        all: bool,
    },
    ConfigSetTimezone {
        timezone: String,
    },
    ConfigGetTimezone,
}

#[derive(Debug)]
pub enum Parse {
    Run(Box<Command>),
    Print(String),
}

#[derive(Debug)]
pub enum Error {
    BrokenPipe,
    Message { message: String, code: u8 },
}

impl Error {
    pub fn usage(message: impl Into<String>) -> Self {
        Self::Message {
            message: message.into(),
            code: 2,
        }
    }

    fn runtime(message: impl Into<String>) -> Self {
        Self::Message {
            message: message.into(),
            code: 1,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BrokenPipe => formatter.write_str("broken pipe"),
            Self::Message { message, .. } => message.fmt(formatter),
        }
    }
}

impl std::error::Error for Error {}

pub fn main(parse: Result<Parse, Error>) -> ExitCode {
    match parse.and_then(|parsed| match parsed {
        Parse::Run(command) => app::run(*command),
        Parse::Print(text) => io::stdout()
            .lock()
            .write_all(text.as_bytes())
            .map_err(io_error),
    }) {
        Ok(()) | Err(Error::BrokenPipe) => ExitCode::SUCCESS,
        Err(Error::Message { message, code }) => {
            eprintln!("tt: {message}");
            ExitCode::from(code)
        }
    }
}

fn io_error(error: io::Error) -> Error {
    if error.kind() == io::ErrorKind::BrokenPipe {
        return Error::BrokenPipe;
    }
    Error::runtime(error.to_string())
}
