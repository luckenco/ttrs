use clap::{ArgGroup, Parser, Subcommand, error::ErrorKind};

use super::{Command, Error, Parse};

#[derive(Parser)]
#[command(name = "tt", version, about = "Terminal time tracker")]
struct Cli {
    #[command(subcommand)]
    command: Option<TopCommand>,
}

#[derive(Subcommand)]
enum TopCommand {
    /// Start a timer.
    Start {
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
        #[arg(short, long)]
        project: String,
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
enum ProjectCommand {
    /// Add a project.
    Add {
        #[arg(long)]
        name: String,
        #[arg(long)]
        client: Option<String>,
    },
    /// Change a project's client.
    #[command(group(
        ArgGroup::new("client-change")
            .required(true)
            .args(["client", "clear_client"])
    ))]
    Edit {
        #[arg(long)]
        name: String,
        #[arg(long)]
        client: Option<String>,
        #[arg(long)]
        clear_client: bool,
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
            Some(TopCommand::Start { project, scope, at }) => Self::Start { project, scope, at },
            Some(TopCommand::Stop { id, at }) => Self::Stop { id, at },
            Some(TopCommand::Status) => Self::Status,
            Some(TopCommand::Log {
                project,
                scope,
                from,
                to,
            }) => Self::Log {
                project,
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
            Some(TopCommand::Project {
                command: ProjectCommand::Add { name, client },
            }) => Self::ProjectAdd { name, client },
            Some(TopCommand::Project {
                command:
                    ProjectCommand::Edit {
                        name,
                        client,
                        clear_client,
                    },
            }) => Self::ProjectEdit {
                name,
                client,
                clear_client,
            },
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
    fn parses_short_start_options_and_scope_with_spaces() {
        let parsed =
            Cli::try_parse_from(["tt", "start", "-p", "internal", "-s", "Planning meeting"])
                .map(Command::from);

        assert!(matches!(
            parsed,
            Ok(Command::Start {
                project,
                scope,
                at: None
            }) if project == "internal" && scope == "Planning meeting"
        ));
    }

    #[test]
    fn project_edit_requires_one_client_change() {
        assert!(
            Cli::try_parse_from([
                "tt",
                "project",
                "edit",
                "--name",
                "web",
                "--client",
                "Acme",
                "--clear-client",
            ])
            .is_err()
        );
        assert!(Cli::try_parse_from(["tt", "project", "edit", "--name", "web"]).is_err());
    }
}
