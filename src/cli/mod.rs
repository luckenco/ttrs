use std::{fmt, io, io::Write, process::ExitCode};

mod app;
pub mod clap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    Bare,
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
    Log {
        project: String,
        scope: String,
        from: String,
        to: String,
    },
    ClientAdd {
        name: String,
    },
    ClientList,
    ProjectAdd {
        name: String,
        client: Option<String>,
    },
    ProjectEdit {
        name: String,
        client: Option<String>,
        clear_client: bool,
    },
    ProjectList,
    ConfigSetTimezone {
        timezone: String,
    },
    ConfigGetTimezone,
}

#[derive(Debug)]
pub enum Parse {
    Run(Command),
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
        Parse::Run(command) => app::run(command),
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
