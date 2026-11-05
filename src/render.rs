use crate::{
    cli::{Action, Providers, Request, Sort},
    model::{Provider, Scope},
    query::{Output, execute},
    text::{printable, safe},
    time::local,
};
use anyhow::{Result, bail};
use dialoguer::{Input, Select, theme::ColorfulTheme};
use std::io::{self, IsTerminal, Write};

pub fn output(out: &Output, request: &Request) -> Result<()> {
    let stdout = io::stdout();
    let mut w = io::BufWriter::new(stdout.lock());
    if request.options.json {
        serde_json::to_writer_pretty(&mut w, &out.json(request))?;
        writeln!(w)?;
    } else if request.action == Action::Relations {
        let graph = out.relations.as_ref().unwrap();
        writeln!(
            w,
            "会话关系：{}\n深度 {} · {:?} · {} 个节点 · {} 条关系\n",
            graph.root_id,
            graph.depth,
            graph.direction,
            graph.nodes.len(),
            graph.edges.len()
        )?;
        for node in &graph.nodes {
            writeln!(
                w,
                "{}{}  {}\n  来源 {}{} · 日志 {} · 父关系 {}",
                if node.id == graph.root_id { "* " } else { "  " },
                node.id,
                printable(&node.title, 80),
                safe(&node.source, false),
                node.thread_source
                    .as_ref()
                    .map(|s| format!(" / {}", safe(s, false)))
                    .unwrap_or_default(),
                node.log_status,
                node.parent_status
            )?;
        }
        writeln!(w)?;
        for edge in &graph.edges {
            writeln!(w, "{} --{}--> {}", edge.from, edge.kind.name(), edge.to)?;
            for proof in &edge.evidence {
                writeln!(
                    w,
                    "  {}{} · {}{}",
                    safe(&proof.path, false),
                    proof.line.map(|n| format!(":{n}")).unwrap_or_default(),
                    proof
                        .table
                        .as_ref()
                        .map(|t| format!("{t}."))
                        .unwrap_or_default(),
                    safe(&proof.field, false)
                )?;
            }
        }
        if graph.edges.is_empty() {
            writeln!(
                w,
                "当前本地元数据中没有可确认的关系；这不表示从未发起其他会话。"
            )?;
        }
        for conflict in &graph.conflicts {
            writeln!(
                w,
                "\n冲突：{} 的 {} 父会话，采用 {}",
                conflict.child_id,
                conflict.kind.name(),
                conflict
                    .selected_parent_id
                    .as_deref()
                    .unwrap_or("无（同级来源冲突）")
            )?;
            for candidate in &conflict.candidates {
                writeln!(
                    w,
                    "  候选 {} · {} 条证据",
                    candidate.from,
                    candidate.evidence.len()
                )?;
            }
        }
        if graph.truncated {
            writeln!(w, "\n已达深度限制，还有关系未展开：")?;
            for f in &graph.frontier {
                writeln!(w, "  {} · {:?} · {} 条", f.id, f.direction, f.omitted_edges)?;
            }
        }
    } else if let Some(file) = &out.transcript_file {
        writeln!(w, "{}", file.path.display())?;
    } else if request.action == Action::Show {
        let s = &out.sessions[0];
        writeln!(
            w,
            "# {}\n\nSession: {}\nProject: {}\nCreated: {}\nLatest reply: {}\nSource: {} ({})\n",
            safe(&s.title, false),
            s.id,
            safe(&s.project.name, false),
            local(&s.created_at),
            local(&s.last_reply_at),
            safe(&s.path, false),
            s.provider.name()
        )?;
        coverage(&mut w, s)?;
        for m in &out.messages {
            writeln!(
                w,
                "## {} · {}{}\n\n{}\n",
                if m.role == "user" {
                    "User"
                } else {
                    "Assistant"
                },
                local(&m.timestamp),
                m.phase
                    .as_ref()
                    .map(|p| format!(" · {}", safe(p, false)))
                    .unwrap_or_default(),
                safe(&m.text, true)
            )?;
        }
    } else if request.action == Action::Projects {
        writeln!(w, "Projects: {} · 显示 {}\n", out.total, out.projects.len())?;
        for p in &out.projects {
            writeln!(
                w,
                "{}  ({} 个会话)\n  创建 {} · 回复 {}\n  {}\n",
                safe(&p.project.name, false),
                p.session_count,
                local(&p.latest_created_at),
                local(&p.latest_reply_at),
                safe(
                    p.project
                        .repository
                        .as_deref()
                        .or(p.project.root.as_deref())
                        .unwrap_or("—"),
                    false
                )
            )?;
        }
    } else {
        writeln!(
            w,
            "匹配 {} 个会话 · 显示 {} · 按{}排序\n",
            out.total,
            out.sessions.len(),
            match request.options.sort {
                Sort::Created => "创建时间",
                Sort::Replied => "最新回复",
                Sort::Relevance => "匹配程度",
            }
        )?;
        for (i, s) in out.sessions.iter().enumerate() {
            writeln!(
                w,
                "{}. [{} / {}] {}{}\n   创建 {} · 回复 {}\n   {}",
                i + 1,
                s.provider.name(),
                printable(&s.project.name, 36),
                printable(&s.title, 80),
                if s.is_subagent { " [子代理]" } else { "" },
                local(&s.created_at),
                local(&s.last_reply_at),
                s.id
            )?;
            if !s.matches.is_empty() {
                for hit in &s.matches {
                    let label = match (hit.field.as_str(), hit.role.as_deref()) {
                        ("title", _) => "标题",
                        ("id", _) => "ID",
                        (_, Some("user")) => "用户",
                        (_, Some("assistant")) => "助手",
                        _ => "正文",
                    };
                    writeln!(w, "   ↳ {label}：{}", printable(&hit.text, 180))?;
                }
            } else if !s.last_reply_preview.is_empty() {
                writeln!(w, "   ↳ {}", printable(&s.last_reply_preview, 110))?;
            }
            coverage(&mut w, s)?;
            if !s.details_available && s.provider != Provider::Trae {
                writeln!(w, "   正文暂不可读")?;
            }
            writeln!(w)?;
        }
    }
    w.flush()?;
    if !request.options.json {
        warnings(out);
    }
    if !request.options.json || request.options.timings {
        timings(out);
    }
    Ok(())
}

fn coverage(w: &mut impl Write, s: &crate::model::Session) -> Result<()> {
    if s.provider == Provider::Trae {
        if s.scope == Scope::LocalConversation {
            writeln!(w, "   范围：本地用户/助手正文 · 原生数据库")?;
            return Ok(());
        }
        writeln!(
            w,
            "   范围：{} · 索引来源 {} · 索引更新 {}",
            if s.scope == Scope::IndexedUserQueries {
                "索引中的用户提问（正文未同步）"
            } else {
                "已同步索引轮次的用户/助手正文"
            },
            s.index_source.as_deref().unwrap_or("unknown"),
            s.indexed_at.as_deref().unwrap_or("未知")
        )?;
        if let Some(at) = &s.synced_at {
            writeln!(w, "   正文快照：{at}")?;
        }
    }
    Ok(())
}

fn timings(out: &Output) {
    eprintln!(
        "耗时 {} ms · 正文读取 {} · 元数据读取 {} · 缓存命中 {}",
        out.stats.elapsed_ms,
        out.stats.parsed_files,
        out.stats.header_files,
        out.stats.cached_files
    );
}

fn warnings(out: &Output) {
    for warning in &out.warnings {
        eprintln!("提示：{}", safe(warning, false));
    }
}

fn show(base: &Request, id: String) -> Result<()> {
    by_id(base, id, Action::Show)
}

fn by_id(base: &Request, id: String, action: Action) -> Result<()> {
    let mut request = base.clone();
    request.action = action;
    if action == Action::Relations && request.options.provider == Providers::All {
        request.options.provider = Providers::Codex;
    }
    request.query.clear();
    request.options.id = Some(id);
    request.options.project = None;
    request.options.since = None;
    request.options.until = None;
    request.options.offset = 0;
    request.options.limit = 10;
    request.since = None;
    request.until = None;
    output(&execute(&request)?, &request)
}

fn choose_sessions(base: &Request) -> Result<()> {
    let mut request = base.clone();
    let page_size = base.options.limit.max(10);
    request.options.limit = page_size;
    let mut page = 0usize;
    let theme = ColorfulTheme::default();
    loop {
        request.options.offset = base.options.offset + page as u32 * page_size;
        let result = execute(&request)?;
        warnings(&result);
        timings(&result);
        if result.sessions.is_empty() {
            eprintln!("没有匹配的会话。");
            return Ok(());
        }
        let start = request.options.offset as usize;
        let end = start + result.sessions.len();
        let mut labels: Vec<_> = result
            .sessions
            .iter()
            .map(|s| {
                format!(
                    "[{} / {}] {}  · {}",
                    s.provider.name(),
                    printable(&s.project.name, 25),
                    printable(&s.title, 65),
                    local(if request.options.sort == Sort::Created {
                        &s.created_at
                    } else {
                        &s.last_reply_at
                    })
                )
            })
            .collect();
        let count = labels.len();
        let has_next = end < result.total;
        let has_previous = page > 0;
        if has_next {
            labels.push("下一页 →".into());
        }
        if has_previous {
            labels.push("← 上一页".into());
        }
        labels.push("返回".into());
        let prompt = format!(
            "匹配 {} 个 · {}–{} · ↑↓ 选择，Enter 查看，Esc 返回",
            result.total,
            start + 1,
            end
        );
        let choice = Select::with_theme(&theme)
            .default(0)
            .with_prompt(prompt)
            .items(&labels)
            .max_length(14)
            .interact_opt()?;
        let Some(choice) = choice else { break };
        if choice < count {
            let mut selected = base.clone();
            selected.options.provider = match result.sessions[choice].provider {
                Provider::Codex => Providers::Codex,
                Provider::Trae => Providers::Trae,
                Provider::Traecli => Providers::Traecli,
            };
            if selected.options.provider == Providers::Codex {
                selected.options.trae_ctx = None;
                selected.options.trae_index = None;
            }
            show(&selected, result.sessions[choice].id.clone())?;
            let _: String = Input::with_theme(&theme)
                .with_prompt("按 Enter 返回列表")
                .allow_empty(true)
                .interact_text()?;
        } else if has_next && choice == count {
            page += 1;
        } else if has_previous && choice == count + usize::from(has_next) {
            page -= 1;
        } else {
            break;
        }
    }
    Ok(())
}

pub fn browse(base: &Request) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("browse 需要交互终端；脚本请使用 recent / search / show --json");
    }
    let theme = ColorfulTheme::default();
    eprintln!("session-finder · 本地 Codex / TRAE IDE / TRAE CLI 会话\n");
    loop {
        let choice = Select::with_theme(&theme)
            .default(0)
            .with_prompt("查找会话")
            .items([
                "最近创建",
                "最近回复",
                "按项目浏览",
                "搜索关键词",
                "按 ID 查看",
                "按 ID 查看会话关系",
                "退出",
            ])
            .interact_opt()?;
        let Some(choice) = choice else { return Ok(()) };
        let mut request = base.clone();
        request.action = Action::Search;
        let result = match choice {
            0 | 1 => {
                request.options.sort = if choice == 0 {
                    Sort::Created
                } else {
                    Sort::Replied
                };
                choose_sessions(&request)
            }
            2 => {
                let mut projects_request = request.clone();
                projects_request.action = Action::Projects;
                projects_request.options.limit = 10000;
                let out = execute(&projects_request)?;
                warnings(&out);
                timings(&out);
                let labels: Vec<_> = out
                    .projects
                    .iter()
                    .map(|p| {
                        format!(
                            "{} ({} 个) · {}",
                            printable(&p.project.name, 35),
                            p.session_count,
                            printable(
                                p.project
                                    .repository
                                    .as_deref()
                                    .or(p.project.root.as_deref())
                                    .unwrap_or(""),
                                65
                            )
                        )
                    })
                    .collect();
                if labels.is_empty() {
                    eprintln!("没有匹配的项目。");
                    continue;
                }
                if let Some(index) = Select::with_theme(&theme)
                    .default(0)
                    .with_prompt("选择项目 · Esc 返回")
                    .items(&labels)
                    .max_length(14)
                    .interact_opt()?
                {
                    request.options.project = Some(out.projects[index].project.key.clone());
                    choose_sessions(&request)
                } else {
                    Ok(())
                }
            }
            3 => {
                request.query = Input::with_theme(&theme)
                    .with_prompt("关键词（空输入返回）")
                    .allow_empty(true)
                    .interact_text()?;
                if request.query.trim().is_empty() {
                    continue;
                }
                choose_sessions(&request)
            }
            4 | 5 => {
                let id: String = Input::with_theme(&theme)
                    .with_prompt("UUID、前缀或会话链接（空输入返回）")
                    .allow_empty(true)
                    .interact_text()?;
                if id.trim().is_empty() {
                    continue;
                }
                let Some(id) = crate::text::session_id(&id) else {
                    eprintln!("无效的会话 ID。");
                    continue;
                };
                if choice == 5 {
                    let depth: u32 = Input::with_theme(&theme)
                        .with_prompt("关系深度（1–100）")
                        .default(1)
                        .validate_with(|v: &u32| {
                            if (1..=100).contains(v) {
                                Ok(())
                            } else {
                                Err("请输入 1–100")
                            }
                        })
                        .interact_text()?;
                    request.depth = depth;
                }
                let result = by_id(
                    &request,
                    id,
                    if choice == 5 {
                        Action::Relations
                    } else {
                        Action::Show
                    },
                );
                let _: String = Input::with_theme(&theme)
                    .with_prompt("按 Enter 返回菜单")
                    .allow_empty(true)
                    .interact_text()?;
                result
            }
            _ => return Ok(()),
        };
        if let Err(e) = result {
            eprintln!("错误：{}", safe(&e.to_string(), true));
        }
    }
}
