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

#[derive(Clone, Debug, Default, Args)]
pub struct Options {
    // Resolved by Request::new; Cli keeps omission distinct from explicit `all`.
    #[arg(skip)]
    pub provider: Providers,
    /// TRAE IDE 内 ctx-cli 的路径，用于选择应用；读取直接使用 Rust IPC
    #[arg(long, global = true)]
    pub trae_ctx: Option<PathBuf>,
    /// 离线导出的 TRAE 索引 JSONL 或已有查询快照
    #[arg(long, global = true)]
    pub trae_index: Option<PathBuf>,
    /// TRAE IDE 原生 database.db 路径（默认自动发现）
    #[arg(long, global = true, conflicts_with_all = ["trae_index", "trae_ctx"])]
    pub trae_db: Option<PathBuf>,
    /// 与原生数据库对应的 libai_agent.dylib 路径
    #[arg(long, global = true, conflicts_with_all = ["trae_index", "trae_ctx"])]
    pub trae_library: Option<PathBuf>,
    /// TRAE CLI 私有目录；默认 TRAECLI_HOME、TRAE_HOME/cli 或 ~/.trae/cli
    #[arg(long, global = true)]
    pub traecli_home: Option<PathBuf>,
    /// 项目名、仓库 URL 或 worktree 路径
    #[arg(short, long, global = true)]
    pub project: Option<String>,
    /// UUID、TRAE conversation ID、至少 8 位前缀或会话链接
    #[arg(long, global = true)]
    pub id: Option<String>,
    #[arg(long, value_enum, default_value = "replied", global = true)]
    pub sort: Sort,
    /// 起点：YYYY-MM-DD、ISO 时间、7d、24h、now
    #[arg(long, global = true)]
    pub since: Option<String>,
    /// 终点（ISO 时间不包含边界；纯日期包含当天）
    #[arg(long, global = true)]
    pub until: Option<String>,
    #[arg(short = 'n', long, default_value_t = 10, value_parser = clap::value_parser!(u32).range(1..=10000), global = true)]
    pub limit: u32,
    /// 跳过前 N 条结果，用于分页
    #[arg(long, default_value_t = 0, global = true)]
    pub offset: u32,
    #[arg(long, global = true)]
    pub include_subagents: bool,
    /// 关闭英文拼写和中文跳字容错
    #[arg(long, global = true)]
    pub exact: bool,
    #[arg(long, global = true)]
    pub json: bool,
    /// recent / search JSON 包含完整会话元数据
    #[arg(long, global = true, requires = "json")]
    pub verbose: bool,
    /// 同时向 stderr 输出查询耗时（普通文本及交互菜单默认显示）
    #[arg(long, global = true)]
    pub timings: bool,
    /// 默认 CODEX_HOME 或 ~/.codex
    #[arg(long, global = true)]
    pub home: Option<PathBuf>,
    #[arg(long, global = true)]
    pub cache_dir: Option<PathBuf>,
    /// 重新读取本次查询涉及的日志
    #[arg(long, global = true)]
    pub refresh: bool,
    #[arg(long, global = true)]
    pub no_cache: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// 同步 TRAE 索引中所选会话的用户/助手正文到本地快照
    Sync,
    /// 搜索标题及全部用户/助手文本
    Search { query: Vec<String> },
    /// 最近会话；可附带关键词
    Recent { query: Vec<String> },
    /// 查看对话；TRAE IDE 生成临时 JSONL 并返回路径，Codex/TRAE CLI 输出正文
    Show {
        #[arg(id = "session", value_name = "ID")]
        id: Option<String>,
    },
    /// 查询父会话、fork 和子代理关系（包含子代理，不分页）
    #[command(visible_alias = "related")]
    Relations {
        #[arg(id = "session", value_name = "ID")]
        id: Option<String>,
        /// 向上或向下最多追溯的边数
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=100))]
        depth: u32,
        #[arg(long, value_enum, default_value = "both")]
        direction: Direction,
    },
    /// 项目列表和会话数
    Projects { query: Vec<String> },
    /// 终端菜单：项目、搜索、会话选择
    Browse,
}

#[derive(Debug, Parser)]
#[command(
    version,
    about = "本地 Codex / TRAE IDE / TRAE CLI 会话检索 · Rust CLI",
    after_help = "示例：\n  session-finder recent --sort created\n  session-finder recent -p threadline --since 7d\n  session-finder search \"modren federation\"\n  session-finder search \"thredline\" --provider all --sort relevance\n  session-finder sync --provider trae --id <conversation_id>\n  session-finder show <UUID或前缀> --json\n  session-finder relations <UUID或前缀> --depth 3 --json\n  session-finder browse\n\n直接运行（终端内）进入交互菜单。仅读取来源数据；TRAE 默认读取本机原生数据库，支持普通终端。显式 ContextFS 来源刷新/同步需要 Agent IPC 环境。"
)]
pub struct Cli {
    /// 会话来源：默认 all；relations 默认 codex，sync 需指定 trae
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
                    bail!("sync 当前支持 --provider trae");
                }
                if options.project.is_some()
                    || options.since.is_some()
                    || options.until.is_some()
                    || options.offset != 0
                    || options.limit != 10
                {
                    bail!("sync 使用 --id 选择会话，不接受项目、时间或分页过滤");
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
                        .ok_or_else(|| anyhow::anyhow!("relations 需要有效的 session ID"))?;
                    if let Some(flag) = &options.id
                        && session_id(flag).as_deref() != Some(&parsed)
                    {
                        bail!("会话 ID 参数冲突");
                    }
                    options.id = Some(parsed);
                }
                if options.id.is_none() {
                    bail!("relations 需要 session ID");
                }
                if options.project.is_some() || options.since.is_some() || options.until.is_some() {
                    bail!("relations 按关系查询，不接受 project 或时间过滤");
                }
                if options.offset != 0 || options.limit != 10 {
                    bail!("relations 不分页，请使用 --depth 和 --direction 控制范围");
                }
                (Action::Relations, String::new())
            }
            Some(Command::Show { id }) => {
                if let Some(id) = id {
                    let parsed = session_id(&id)
                        .ok_or_else(|| anyhow::anyhow!("show 需要有效的 session ID"))?;
                    if let Some(flag) = &options.id
                        && session_id(flag).as_deref() != Some(&parsed)
                    {
                        bail!("会话 ID 参数冲突");
                    }
                    options.id = Some(parsed);
                }
                if options.id.is_none() {
                    bail!("show 需要 session ID");
                }
                if options.project.is_some() || options.since.is_some() || options.until.is_some() {
                    bail!("show 按 ID 读取，不接受 project 或时间过滤");
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
            options.id = Some(
                session_id(id)
                    .ok_or_else(|| anyhow::anyhow!("ID 需要 UUID、至少 8 位前缀或会话链接"))?,
            );
        }
        if action == Action::Projects && options.id.is_some() {
            bail!("projects 不接受 session ID");
        }
        if action == Action::Relations && options.provider != Providers::Codex {
            bail!("relations 当前只支持 --provider codex；TRAE 轮次 ID 不是子会话关系");
        }
        if !options.provider.trae()
            && (options.trae_ctx.is_some()
                || options.trae_index.is_some()
                || options.trae_db.is_some()
                || options.trae_library.is_some())
        {
            bail!("TRAE 参数需要 --provider trae 或 all");
        }
        if !options.provider.traecli() && options.traecli_home.is_some() {
            bail!("--traecli-home 需要 --provider traecli 或 all");
        }
        if action == Action::Browse && options.json {
            bail!("交互菜单不支持 --json，请使用 recent / search / show");
        }
        if options.verbose && action != Action::Search {
            bail!("--verbose 仅用于 recent / search 的 --json 列表");
        }
        let now = chrono::Utc::now().timestamp_millis();
        let since = boundary(options.since.as_deref(), false, now)?;
        let until = boundary(options.until.as_deref(), true, now)?;
        if since.zip(until).is_some_and(|(a, b)| a >= b) {
            bail!("--since 必须早于 --until");
        }
        if options.sort == Sort::Relevance && (since.is_some() || until.is_some()) {
            bail!("时间范围需搭配 --sort created 或 replied");
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
