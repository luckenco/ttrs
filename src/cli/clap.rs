use clap::{ArgGroup, Args, Parser, Subcommand, error::ErrorKind};

use super::{Command, Error, Parse, TrackingTarget};

#[derive(Parser)]
#[command(name = "tt", version, about = "Terminal time tracker")]
struct Cli {
    #[command(subcommand)]
    command: Option<TopCommand>,
}

#[derive(Args)]
#[command(group(
    ArgGroup::new("target")
        .required(true)
        .args(["project", "agreement"])
))]
struct TargetArgs {
    /// Project name or id:<number>; uses its current agreement.
    #[arg(short, long, conflicts_with = "agreement")]
    project: Option<String>,
    /// Active agreement reference in id:<number> form.
    #[arg(short = 'a', long)]
    agreement: Option<String>,
}

impl TargetArgs {
    fn into_target(self) -> TrackingTarget {
        match (self.project, self.agreement) {
            (Some(project), None) => TrackingTarget::Project(project),
            (None, Some(agreement)) => TrackingTarget::Agreement(agreement),
            _ => unreachable!("clap enforces the tracking target"),
        }
    }
}

#[derive(Args)]
struct AgreementTermsArgs {
    /// Hourly price in currency units, with up to six decimal places.
    #[arg(long)]
    rate: String,
    /// Three-letter uppercase currency code.
    #[arg(long)]
    currency: String,
    #[arg(long)]
    minimum_hours: Option<String>,
    #[arg(long)]
    maximum_hours: Option<String>,
    /// First local calendar day governed by these terms.
    #[arg(long)]
    effective_on: String,
}

#[derive(Subcommand)]
enum TopCommand {
    /// Start a timer.
    Start {
        /// Project name or id:<number>.
        #[arg(short, long)]
        project: String,
        #[arg(short, long)]
        scope: String,
        #[arg(long)]
        at: Option<String>,
    },
    /// Stop a running timer.
    Stop {
        #[arg(long)]
        id: String,
        #[arg(long)]
        at: Option<String>,
    },
    /// Show running timers.
    Status,
    /// Log a completed entry.
    Log {
        #[command(flatten)]
        target: TargetArgs,
        #[arg(short, long)]
        scope: String,
        #[arg(short, long)]
        from: String,
        #[arg(short = 'e', long = "to")]
        to: String,
    },
    /// Manage clients.
    Client {
        #[command(subcommand)]
        command: ClientCommand,
    },
    /// Manage service agreements.
    Agreement {
        #[command(subcommand)]
        command: AgreementCommand,
    },
    /// Manage projects.
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// Manage configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}

#[derive(Subcommand)]
enum ClientCommand {
    /// Add a client.
    Add {
        #[arg(long)]
        name: String,
    },
    /// List clients.
    List,
}

#[derive(Subcommand)]
enum AgreementCommand {
    /// Replace a project's current agreement with immutable terms.
    Add {
        /// Project name or id:<number>.
        #[arg(long)]
        project: String,
        #[arg(long)]
        name: String,
        #[command(flatten)]
        terms: AgreementTermsArgs,
    },
    /// List service agreements.
    List {
        /// Client name or id:<number>.
        #[arg(long)]
        client: Option<String>,
        /// Include replaced agreements and agreements of archived projects.
        #[arg(long)]
        all: bool,
    },
}

#[derive(Subcommand)]
enum ProjectCommand {
    /// Add a project and its initial agreement atomically.
    Add {
        #[arg(long)]
        name: String,
        /// Client name or id:<number>.
        #[arg(long)]
        client: String,
        /// IANA timezone defining calendar-month boundaries.
        #[arg(long)]
        timezone: String,
        /// Initial agreement name.
        #[arg(long)]
        agreement: String,
        #[command(flatten)]
        terms: AgreementTermsArgs,
    },
    /// Lock a project after its final active calendar day.
    Archive {
        /// Project name or id:<number>.
        #[arg(long)]
        project: String,
        /// Final active date; defaults to today in the project timezone.
        #[arg(long)]
        on: Option<String>,
    },
    /// Restore an archived project and continuous commercial coverage.
    Unarchive {
        /// Project name or id:<number>.
        #[arg(long)]
        project: String,
    },
    /// List projects.
    List {
        /// Include archived projects.
        #[arg(long)]
        all: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Set a configuration value.
    Set {
        #[command(subcommand)]
        key: SetConfig,
    },
    /// Get a configuration value.
    Get {
        #[command(subcommand)]
        key: GetConfig,
    },
}

#[derive(Subcommand)]
enum SetConfig {
    /// Set the IANA timezone.
    Timezone { timezone: String },
}

#[derive(Subcommand)]
enum GetConfig {
    /// Show the active timezone.
    Timezone,
}

pub fn parse_env() -> Result<Parse, Error> {
    parse(Cli::try_parse())
}

fn parse(result: Result<Cli, clap::Error>) -> Result<Parse, Error> {
    match result {
        Ok(cli) => Ok(Parse::Run(cli.into())),
        Err(info)
            if matches!(
                info.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            Ok(Parse::Print(info.to_string()))
        }
        Err(error) => Err(Error::usage(error.to_string())),
    }
}

impl From<Cli> for Command {
    fn from(cli: Cli) -> Self {
        match cli.command {
            None => Self::Bare,
            Some(TopCommand::Start { project, scope, at }) => Self::Start { project, scope, at },
            Some(TopCommand::Stop { id, at }) => Self::Stop { id, at },
            Some(TopCommand::Status) => Self::Status,
            Some(TopCommand::Log {
                target,
                scope,
                from,
                to,
            }) => Self::Log {
                target: target.into_target(),
                scope,
                from,
                to,
            },
            Some(TopCommand::Client {
                command: ClientCommand::Add { name },
            }) => Self::ClientAdd { name },
            Some(TopCommand::Client {
                command: ClientCommand::List,
            }) => Self::ClientList,
            Some(TopCommand::Agreement {
                command:
                    AgreementCommand::Add {
                        project,
                        name,
                        terms:
                            AgreementTermsArgs {
                                rate,
                                currency,
                                minimum_hours,
                                maximum_hours,
                                effective_on,
                            },
                    },
            }) => Self::AgreementAdd {
                project,
                name,
                rate,
                currency,
                minimum_hours,
                maximum_hours,
                effective_on,
            },
            Some(TopCommand::Agreement {
                command: AgreementCommand::List { client, all },
            }) => Self::AgreementList { client, all },
            Some(TopCommand::Project {
                command:
                    ProjectCommand::Add {
                        name,
                        client,
                        timezone,
                        agreement,
                        terms:
                            AgreementTermsArgs {
                                rate,
                                currency,
                                minimum_hours,
                                maximum_hours,
                                effective_on,
                            },
                    },
            }) => Self::ProjectAdd {
                name,
                client,
                agreement,
                rate,
                currency,
                minimum_hours,
                maximum_hours,
                timezone,
                effective_on,
            },
            Some(TopCommand::Project {
                command: ProjectCommand::Archive { project, on },
            }) => Self::ProjectArchive { project, on },
            Some(TopCommand::Project {
                command: ProjectCommand::Unarchive { project },
            }) => Self::ProjectUnarchive { project },
            Some(TopCommand::Project {
                command: ProjectCommand::List { all },
            }) => Self::ProjectList { all },
            Some(TopCommand::Config {
                command:
                    ConfigCommand::Set {
                        key: SetConfig::Timezone { timezone },
                    },
            }) => Self::ConfigSetTimezone { timezone },
            Some(TopCommand::Config {
                command:
                    ConfigCommand::Get {
                        key: GetConfig::Timezone,
                    },
            }) => Self::ConfigGetTimezone,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_project_and_agreement_targets() {
        let project =
            Cli::try_parse_from(["tt", "start", "-p", "website", "-s", "Planning meeting"])
                .map(Command::from);
        assert!(matches!(
            project,
            Ok(Command::Start { project, .. }) if project == "website"
        ));

        let agreement = Cli::try_parse_from([
            "tt",
            "log",
            "--agreement",
            "id:1",
            "--scope",
            "Call",
            "--from",
            "0900",
            "--to",
            "1000",
        ])
        .map(Command::from);
        assert!(matches!(
            agreement,
            Ok(Command::Log {
                target: TrackingTarget::Agreement(agreement),
                ..
            }) if agreement == "id:1"
        ));
    }
}
