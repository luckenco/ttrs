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
    /// Use the project's active agreement.
    #[arg(short, long, conflicts_with_all = ["agreement", "client"])]
    project: Option<String>,
    /// Record agreement-level time without a project.
    #[arg(short = 'a', long, requires = "client")]
    agreement: Option<String>,
    /// Client owning an agreement-level target.
    #[arg(short = 'c', long, requires = "agreement")]
    client: Option<String>,
}

impl TargetArgs {
    fn into_target(self) -> TrackingTarget {
        match (self.project, self.client, self.agreement) {
            (Some(project), None, None) => TrackingTarget::Project(project),
            (None, Some(client), Some(agreement)) => {
                TrackingTarget::Agreement { client, agreement }
            }
            _ => unreachable!("clap enforces the tracking target"),
        }
    }
}

#[derive(Subcommand)]
enum TopCommand {
    /// Start a timer.
    Start {
        #[command(flatten)]
        target: TargetArgs,
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
    /// Add an immutable service agreement.
    Add {
        #[arg(long)]
        client: String,
        #[arg(long)]
        name: String,
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
        /// IANA timezone defining calendar-month boundaries.
        #[arg(long)]
        timezone: String,
    },
    /// List service agreements.
    List {
        #[arg(long)]
        client: Option<String>,
    },
    /// Prevent new work under an unused agreement.
    Archive {
        #[arg(long)]
        client: String,
        #[arg(long)]
        name: String,
    },
}

#[derive(Subcommand)]
enum ProjectCommand {
    /// Add a project and select its active agreement.
    Add {
        #[arg(long)]
        name: String,
        #[arg(long)]
        client: String,
        #[arg(long)]
        agreement: String,
    },
    /// Switch the agreement used for new project work.
    Edit {
        #[arg(long)]
        name: String,
        #[arg(long)]
        agreement: String,
    },
    /// List projects.
    List,
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
            Some(TopCommand::Start { target, scope, at }) => Self::Start {
                target: target.into_target(),
                scope,
                at,
            },
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
                        client,
                        name,
                        rate,
                        currency,
                        minimum_hours,
                        maximum_hours,
                        timezone,
                    },
            }) => Self::AgreementAdd {
                client,
                name,
                rate,
                currency,
                minimum_hours,
                maximum_hours,
                timezone,
            },
            Some(TopCommand::Agreement {
                command: AgreementCommand::List { client },
            }) => Self::AgreementList { client },
            Some(TopCommand::Agreement {
                command: AgreementCommand::Archive { client, name },
            }) => Self::AgreementArchive { client, name },
            Some(TopCommand::Project {
                command:
                    ProjectCommand::Add {
                        name,
                        client,
                        agreement,
                    },
            }) => Self::ProjectAdd {
                name,
                client,
                agreement,
            },
            Some(TopCommand::Project {
                command: ProjectCommand::Edit { name, agreement },
            }) => Self::ProjectEdit { name, agreement },
            Some(TopCommand::Project {
                command: ProjectCommand::List,
            }) => Self::ProjectList,
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
            Ok(Command::Start {
                target: TrackingTarget::Project(project),
                ..
            }) if project == "website"
        ));

        let agreement = Cli::try_parse_from([
            "tt",
            "start",
            "--client",
            "Acme",
            "--agreement",
            "Support",
            "--scope",
            "Call",
        ])
        .map(Command::from);
        assert!(matches!(
            agreement,
            Ok(Command::Start {
                target: TrackingTarget::Agreement { client, agreement },
                ..
            }) if client == "Acme" && agreement == "Support"
        ));
    }
}
