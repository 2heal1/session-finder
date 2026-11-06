use crate::{text::session_id, time::boundary};
use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    #[default]
    Both,
    Parents,
    Children,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum Sort {
    Created,
    #[default]
    Replied,
    Relevance,
}
impl Sort {
    pub fn name(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Replied => "replied",
            Self::Relevance => "relevance",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum Providers {
    Codex,
    Trae,
    Traecli,
    #[default]
    All,
}

impl Providers {
    pub fn codex(self) -> bool {
        matches!(self, Self::Codex | Self::All)
    }
    pub fn trae(self) -> bool {
        matches!(self, Self::Trae | Self::All)
    }
    pub fn traecli(self) -> bool {
        matches!(self, Self::Traecli | Self::All)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum Decoration {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Clone, Debug, Default, Args)]
pub struct Options {
    // Resolved by Request::new; Cli keeps omission distinct from explicit `all`.
    #[arg(skip)]
    pub provider: Providers,
    /// TRAE IDE ctx-cli path for application selection; reads use native Rust IPC
    #[arg(long, global = true)]
    pub trae_ctx: Option<PathBuf>,
    /// Offline TRAE index JSONL export or existing query snapshot
    #[arg(long, global = true)]
    pub trae_index: Option<PathBuf>,
    /// Native TRAE IDE database.db path (auto-discovered by default)
    #[arg(long, global = true, conflicts_with_all = ["trae_index", "trae_ctx"])]
    pub trae_db: Option<PathBuf>,
    /// libai_agent.dylib path matching the native database
    #[arg(long, global = true, conflicts_with_all = ["trae_index", "trae_ctx"])]
    pub trae_library: Option<PathBuf>,
    /// TRAE CLI data directory; defaults to TRAECLI_HOME, TRAE_HOME/cli, or ~/.trae/cli
    #[arg(long, global = true)]
    pub traecli_home: Option<PathBuf>,
    /// Project name, repository URL, or worktree path
    #[arg(short, long, global = true)]
    pub project: Option<String>,
    /// UUID, TRAE conversation ID, prefix of at least 8 characters, or session link
    #[arg(long, global = true)]
    pub id: Option<String>,
    #[arg(long, value_enum, default_value = "replied", global = true)]
    pub sort: Sort,
    /// Start: YYYY-MM-DD, ISO timestamp, 7d, 24h, or now
    #[arg(long, global = true)]
    pub since: Option<String>,
    /// End: exclusive for ISO timestamps, inclusive for calendar dates
    #[arg(long, global = true)]
    pub until: Option<String>,
    #[arg(short = 'n', long, default_value_t = 10, value_parser = clap::value_parser!(u32).range(1..=10000), global = true)]
    pub limit: u32,
    /// Skip the first N results for pagination
    #[arg(long, default_value_t = 0, global = true)]
    pub offset: u32,
    #[arg(long, global = true)]
    pub include_subagents: bool,
    /// Disable English typo tolerance and Chinese character-gap matching
    #[arg(long, global = true)]
    pub exact: bool,
    #[arg(long, global = true)]
    pub json: bool,
    /// Include full session metadata in recent / search JSON lists
    #[arg(long, global = true, requires = "json")]
    pub verbose: bool,
    /// Also print query timing to stderr (shown by default for text and interactive output)
    #[arg(long, global = true)]
    pub timings: bool,
    /// Terminal colors; auto respects NO_COLOR and disables colors on redirected output
    #[arg(long, value_enum, default_value = "auto", global = true)]
    pub color: Decoration,
    /// OSC 8 links on session IDs; auto enables links only in terminals
    #[arg(long, value_enum, default_value = "auto", global = true)]
    pub links: Decoration,
    /// Disable the terminal loading status (final query timing is still shown)
    #[arg(long, global = true)]
    pub no_progress: bool,
    /// Defaults to CODEX_HOME or ~/.codex
    #[arg(long, global = true)]
    pub home: Option<PathBuf>,
    #[arg(long, global = true)]
    pub cache_dir: Option<PathBuf>,
    /// Reread the logs involved in this query
    #[arg(long, global = true)]
    pub refresh: bool,
    #[arg(long, global = true)]
    pub no_cache: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Sync user/assistant text for selected TRAE indexed conversations to local snapshots
    Sync,
    /// Search titles and all user/assistant text
    Search { query: Vec<String> },
    /// Recent sessions, optionally filtered by keywords
    Recent { query: Vec<String> },
    /// Show a conversation; TRAE IDE returns a temporary JSONL path, Codex/TRAE CLI print text
    Show {
        #[arg(id = "session", value_name = "ID")]
        id: Option<String>,
    },
    /// Inspect parent, fork, and subagent relationships (includes subagents, no pagination)
    #[command(visible_alias = "related")]
    Relations {
        #[arg(id = "session", value_name = "ID")]
        id: Option<String>,
        /// Maximum number of edges to traverse upward or downward
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=100))]
        depth: u32,
        #[arg(long, value_enum, default_value = "both")]
        direction: Direction,
    },
    /// List projects and session counts
    Projects { query: Vec<String> },
    /// Terminal menu for projects, search, and session selection
    Browse,
}

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Local Codex / TRAE IDE / TRAE CLI session search · Rust CLI",
    after_help = "Examples:\n  session-finder recent --sort created\n  session-finder recent -p threadline --since 7d\n  session-finder search \"modren federation\"\n  session-finder search \"thredline\" --provider all --sort relevance\n  session-finder sync --provider trae --id <conversation_id>\n  session-finder show <UUID-or-prefix> --json\n  session-finder relations <UUID-or-prefix> --depth 3 --json\n  session-finder browse\n\nRun without arguments in a terminal to open the interactive menu. Source data stays read-only. TRAE reads the local native database by default and works in ordinary terminals. Explicit ContextFS refresh/sync operations require the Agent IPC environment."
)]
pub struct Cli {
    /// Session provider: defaults to all; relations defaults to codex, sync requires trae
    #[arg(long, value_enum, global = true)]
    pub provider: Option<Providers>,
    #[command(flatten)]
    pub options: Options,
    #[command(subcommand)]
    pub command: Option<Command>,
    pub words: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Sync,
    Search,
    Projects,
    Show,
    Relations,
    Browse,
}

#[derive(Clone, Debug)]
pub struct Request {
    pub options: Options,
    pub query: String,
    pub action: Action,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub depth: u32,
    pub direction: Direction,
}

impl Request {
    pub fn new(cli: Cli, interactive_default: bool) -> Result<Self> {
        let mut options = cli.options;
        options.provider =
            cli.provider
                .unwrap_or(if matches!(cli.command, Some(Command::Relations { .. })) {
                    Providers::Codex
                } else {
                    Providers::All
                });
        let mut depth = 1;
        let mut direction = Direction::Both;
        let (action, mut query) = match cli.command {
            Some(Command::Sync) => {
                if options.provider != Providers::Trae {
                    bail!("sync currently supports --provider trae");
                }
                if options.project.is_some()
                    || options.since.is_some()
                    || options.until.is_some()
                    || options.offset != 0
                    || options.limit != 10
                {
                    bail!(
                        "sync selects sessions with --id; project, time, and pagination filters are not supported"
                    );
                }
                (Action::Sync, String::new())
            }
            Some(Command::Relations {
                id,
                depth: requested_depth,
                direction: requested_direction,
            }) => {
                depth = requested_depth;
                direction = requested_direction;
                if let Some(id) = id {
                    let parsed = session_id(&id)
                        .ok_or_else(|| anyhow::anyhow!("relations requires a valid session ID"))?;
                    if let Some(flag) = &options.id
                        && session_id(flag).as_deref() != Some(&parsed)
                    {
                        bail!("Conflicting session ID arguments");
                    }
                    options.id = Some(parsed);
                }
                if options.id.is_none() {
                    bail!("relations requires a session ID");
                }
                if options.project.is_some() || options.since.is_some() || options.until.is_some() {
                    bail!(
                        "relations queries relationships; project and time filters are not supported"
                    );
                }
                if options.offset != 0 || options.limit != 10 {
                    bail!(
                        "relations does not paginate; use --depth and --direction to control traversal"
                    );
                }
                (Action::Relations, String::new())
            }
            Some(Command::Show { id }) => {
                if let Some(id) = id {
                    let parsed = session_id(&id)
                        .ok_or_else(|| anyhow::anyhow!("show requires a valid session ID"))?;
                    if let Some(flag) = &options.id
                        && session_id(flag).as_deref() != Some(&parsed)
                    {
                        bail!("Conflicting session ID arguments");
                    }
                    options.id = Some(parsed);
                }
                if options.id.is_none() {
                    bail!("show requires a session ID");
                }
                if options.project.is_some() || options.since.is_some() || options.until.is_some() {
                    bail!("show reads by ID; project and time filters are not supported");
                }
                (Action::Show, String::new())
            }
            Some(Command::Projects { query }) => (Action::Projects, query.join(" ")),
            Some(Command::Browse) => (Action::Browse, String::new()),
            Some(Command::Recent { query } | Command::Search { query }) => {
                (Action::Search, query.join(" "))
            }
            None if interactive_default => (Action::Browse, String::new()),
            None => (Action::Search, cli.words.join(" ")),
        };
        if options.id.is_none()
            && action == Action::Search
            && let Some(id) = session_id(&query)
        {
            options.id = Some(id);
            query.clear();
        }
        if let Some(id) = &options.id {
            options.id = Some(session_id(id).ok_or_else(|| {
                anyhow::anyhow!(
                    "ID requires a UUID, prefix of at least 8 characters, or session link"
                )
            })?);
        }
        if action == Action::Projects && options.id.is_some() {
            bail!("projects does not accept a session ID");
        }
        if action == Action::Relations && options.provider != Providers::Codex {
            bail!(
                "relations currently supports only --provider codex; TRAE turn IDs do not establish sub-session relationships"
            );
        }
        if !options.provider.trae()
            && (options.trae_ctx.is_some()
                || options.trae_index.is_some()
                || options.trae_db.is_some()
                || options.trae_library.is_some())
        {
            bail!("TRAE options require --provider trae or all");
        }
        if !options.provider.traecli() && options.traecli_home.is_some() {
            bail!("--traecli-home requires --provider traecli or all");
        }
        if action == Action::Browse && options.json {
            bail!("The interactive menu does not support --json; use recent / search / show");
        }
        if options.verbose && action != Action::Search {
            bail!("--verbose is supported only for recent / search --json lists");
        }
        let now = chrono::Utc::now().timestamp_millis();
        let since = boundary(options.since.as_deref(), false, now)?;
        let until = boundary(options.until.as_deref(), true, now)?;
        if since.zip(until).is_some_and(|(a, b)| a >= b) {
            bail!("--since must be earlier than --until");
        }
        if options.sort == Sort::Relevance && (since.is_some() || until.is_some()) {
            bail!("Time filters require --sort created or replied");
        }
        Ok(Self {
            options,
            query,
            action,
            since,
            until,
            depth,
            direction,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_provider_searches_all_and_explicit_values_take_priority() {
        for args in [
            vec!["sf"],
            vec!["sf", "needle"],
            vec!["sf", "search", "needle"],
            vec!["sf", "recent"],
            vec!["sf", "projects"],
            vec!["sf", "browse"],
            vec!["sf", "show", "01000000"],
            vec!["sf", "--id", "01000000"],
        ] {
            let request = Request::new(Cli::try_parse_from(&args).unwrap(), false).unwrap();
            assert_eq!(request.options.provider, Providers::All, "{args:?}");
        }
        let interactive = Request::new(Cli::try_parse_from(["sf"]).unwrap(), true).unwrap();
        assert_eq!(interactive.action, Action::Browse);
        assert_eq!(interactive.options.provider, Providers::All);
        for (name, provider) in [
            ("codex", Providers::Codex),
            ("trae", Providers::Trae),
            ("traecli", Providers::Traecli),
            ("all", Providers::All),
        ] {
            for args in [
                vec!["sf", "--provider", name, "search", "needle"],
                vec!["sf", "search", "needle", "--provider", name],
            ] {
                let request = Request::new(Cli::try_parse_from(args).unwrap(), false).unwrap();
                assert_eq!(request.options.provider, provider);
            }
        }
    }

    #[test]
    fn relation_defaults_and_sync_source_requirements_are_preserved() {
        for action in ["relations", "related"] {
            let req = Request::new(
                Cli::try_parse_from(["sf", action, "01000000"]).unwrap(),
                false,
            )
            .unwrap();
            assert_eq!(req.options.provider, Providers::Codex);
            let cli = Cli::try_parse_from(["sf", action, "01000000", "--provider", "all"]).unwrap();
            assert!(Request::new(cli, false).is_err());
        }
        assert!(Request::new(Cli::try_parse_from(["sf", "sync"]).unwrap(), false).is_err());
        let req = Request::new(
            Cli::try_parse_from(["sf", "sync", "--provider", "trae"]).unwrap(),
            false,
        )
        .unwrap();
        assert_eq!(req.options.provider, Providers::Trae);
    }
}
