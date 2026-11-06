use crate::{
    cli::{Action, Decoration, Providers, Request, Sort},
    model::{Provider, Scope},
    query::{Event, LiveResults, Output, execute_incremental, execute_observed},
    text::{printable, safe},
    time::local,
};
use anyhow::{Result, bail};
use dialoguer::{Input, Select, theme::ColorfulTheme};
use std::io::{self, IsTerminal, Write};

const ACCENT: &str = "38;5;163";
const MATCH: &str = "1;38;5;129";

struct Style {
    color: bool,
    links: bool,
}

impl Style {
    fn new(request: &Request) -> Self {
        let terminal =
            io::stdout().is_terminal() && !std::env::var("TERM").is_ok_and(|v| v == "dumb");
        let enabled = |mode, auto| match mode {
            Decoration::Auto => auto,
            Decoration::Always => true,
            Decoration::Never => false,
        };
        Self {
            color: !request.options.json
                && enabled(
                    request.options.color,
                    terminal && std::env::var_os("NO_COLOR").is_none(),
                ),
            links: !request.options.json && enabled(request.options.links, terminal),
        }
    }

    fn paint(&self, text: &str, code: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.into()
        }
    }

    fn highlight(&self, text: &str, request: &Request) -> String {
        if !self.color || request.query.is_empty() {
            return text.into();
        }
        let mut result = String::new();
        let mut cursor = 0;
        for span in crate::snippets::highlight_ranges(text, &request.query, request.options.exact) {
            result.push_str(&text[cursor..span.start]);
            result.push_str(&self.paint(&text[span.clone()], MATCH));
            cursor = span.end;
        }
        result.push_str(&text[cursor..]);
        result
    }

    fn session_id(&self, session: &crate::model::Session) -> String {
        let id = self.paint(&safe(&session.id, false), ACCENT);
        if self.links
            && let Some(url) = session.open_url()
        {
            return format!("\x1b]8;;{url}\x1b\\{id}\x1b]8;;\x1b\\");
        }
        id
    }
}

fn list_header(w: &mut impl Write, total: usize, shown: usize, request: &Request) -> Result<()> {
    writeln!(
        w,
        "{}\n",
        Style::new(request).paint(
            &format!(
                "Matched {total} sessions · Showing {shown} · Sorted by {}",
                match request.options.sort {
                    Sort::Created => "creation time",
                    Sort::Replied => "latest reply",
                    Sort::Relevance => "relevance",
                }
            ),
            "1"
        )
    )?;
    Ok(())
}

fn list_session(
    w: &mut impl Write,
    index: usize,
    s: &crate::model::Session,
    request: &Request,
) -> Result<()> {
    let style = Style::new(request);
    writeln!(
        w,
        "{}. [{} / {}] {}{}\n   Created {} · Replied {}\n   Session ID: {}",
        index + 1,
        style.paint(s.provider.name(), ACCENT),
        printable(&s.project.name, 36),
        style.paint(&style.highlight(&printable(&s.title, 80), request), "1"),
        if s.is_subagent { " [subagent]" } else { "" },
        local(&s.created_at),
        local(&s.last_reply_at),
        style.session_id(s)
    )?;
    for hit in &s.matches {
        let label = match (hit.field.as_str(), hit.role.as_deref()) {
            ("title", _) => "Title",
            ("id", _) => "ID",
            (_, Some("user")) => "User",
            (_, Some("assistant")) => "Assistant",
            _ => "Body",
        };
        writeln!(
            w,
            "   ↳ {label}: {}",
            style.highlight(&printable(&hit.text, 180), request)
        )?;
    }
    if s.matches.is_empty() && !s.last_reply_preview.is_empty() {
        writeln!(w, "   ↳ {}", printable(&s.last_reply_preview, 110))?;
    }
    coverage(w, s)?;
    if !s.details_available && s.provider != Provider::Trae {
        writeln!(w, "   Body currently unavailable")?;
    }
    writeln!(w)?;
    Ok(())
}

/// Redraw only our bounded provisional region; never change terminal modes.
#[derive(Default)]
struct LiveTerminal {
    lines: usize,
}

impl LiveTerminal {
    fn draw(
        &mut self,
        w: &mut impl Write,
        total: usize,
        sessions: &[crate::model::Session],
        request: &Request,
    ) -> Result<()> {
        use dialoguer::console::{Term, measure_text_width, truncate_str};
        self.finish(w)?;
        let (rows, columns) = Term::stdout().size();
        let columns = usize::from(columns).max(1);
        let mut remaining = usize::from(rows).saturating_sub(5);
        let style = Style::new(request);
        writeln!(
            w,
            "{}\n",
            truncate_str(
                &format!("Searching · {total} matches so far · Results and order may change"),
                columns,
                ""
            )
        )?;
        let mut displayed = 0;
        let mut lines = 4;
        for (index, s) in sessions.iter().enumerate() {
            let title = format!(
                "{}. [{}] {}",
                index + 1,
                s.provider.name(),
                printable(&s.title, 80)
            );
            let id = format!("   Session ID: {}", style.session_id(s));
            let height = 2 + measure_text_width(&id).saturating_sub(1) / columns;
            if height > remaining {
                break;
            }
            writeln!(
                w,
                "{}\n{}",
                style.highlight(&truncate_str(&title, columns, "…"), request),
                id
            )?;
            remaining -= height;
            lines += height;
            displayed += 1;
        }
        writeln!(
            w,
            "\n{}",
            truncate_str(
                &format!(
                    "Visible {displayed}/{} · Full page follows when complete",
                    sessions.len()
                ),
                columns,
                ""
            )
        )?;
        self.lines = lines;
        w.flush()?;
        Ok(())
    }

    fn finish(&mut self, w: &mut impl Write) -> Result<()> {
        if self.lines > 0 {
            let rows = usize::from(dialoguer::console::Term::stdout().size().0);
            write!(
                w,
                "\r\x1b[{}A\x1b[J",
                self.lines.min(rows.saturating_sub(1)).max(1)
            )?;
            w.flush()?;
            self.lines = 0;
        }
        Ok(())
    }
}

impl Drop for LiveTerminal {
    fn drop(&mut self) {
        let _ = self.finish(&mut io::stdout().lock());
    }
}

pub fn run(request: &Request) -> Result<()> {
    let streaming = request.action == Action::Search
        && !request.options.json
        && io::stdout().is_terminal()
        && !std::env::var("TERM").is_ok_and(|v| v == "dumb");
    let start = std::time::Instant::now();
    let mut live = LiveTerminal::default();
    let mut candidates = LiveResults::default();
    let mut last_draw = None;
    let mut loading = crate::progress::Loading::start_since(request, start);
    let stdout = io::stdout();
    let mut writer = io::BufWriter::new(stdout.lock());
    let mut observe = |event: Event<'_>| {
        match event {
            Event::Phase(phase) => {
                if streaming && loading.is_none() {
                    loading = crate::progress::Loading::start_since(request, start);
                }
                if let Some(loading) = &loading {
                    loading.phase(phase);
                }
            }
            Event::Results { total, shown } if streaming => {
                loading.take();
                live.finish(&mut writer)?;
                list_header(&mut writer, total, shown, request)?;
                writer.flush()?;
            }
            Event::Session { index, session } if streaming => {
                loading.take();
                list_session(&mut writer, index, session, request)?;
                writer.flush()?;
            }
            Event::Candidate { session } if streaming => {
                candidates.update(session);
                if last_draw.is_none_or(|time: std::time::Instant| {
                    time.elapsed() >= std::time::Duration::from_millis(50)
                }) {
                    let (total, page) = candidates.page(request);
                    if total > 0 || live.lines > 0 {
                        loading.take();
                        live.draw(&mut writer, total, &page, request)?;
                        last_draw = Some(std::time::Instant::now());
                        loading = crate::progress::Loading::start_since(request, start);
                        if let Some(loading) = &loading {
                            loading.phase("Reading and matching");
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    };
    let result = if streaming {
        execute_incremental(request, &mut observe)
    } else {
        execute_observed(request, &mut observe)
    };
    drop(loading);
    drop(writer);
    drop(live);
    let out = result?;
    if streaming {
        warnings(&out);
        timings(&out);
        Ok(())
    } else {
        output(&out, request)
    }
}

fn execute(request: &Request) -> Result<Output> {
    let loading = crate::progress::Loading::start(request);
    execute_observed(request, &mut |event| {
        if let Event::Phase(phase) = event
            && let Some(loading) = &loading
        {
            loading.phase(phase);
        }
        Ok(())
    })
}

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
            "Session relationships: {}\nDepth {} · {:?} · {} nodes · {} edges\n",
            graph.root_id,
            graph.depth,
            graph.direction,
            graph.nodes.len(),
            graph.edges.len()
        )?;
        for node in &graph.nodes {
            writeln!(
                w,
                "{}{}  {}\n  Source {}{} · Log {} · Parent {}",
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
                "No confirmed relationships in local metadata; other sessions may still have been started."
            )?;
        }
        for conflict in &graph.conflicts {
            writeln!(
                w,
                "\nConflict: {} has competing {} parents; selected {}",
                conflict.child_id,
                conflict.kind.name(),
                conflict
                    .selected_parent_id
                    .as_deref()
                    .unwrap_or("none (conflicting sources of equal priority)")
            )?;
            for candidate in &conflict.candidates {
                writeln!(
                    w,
                    "  Candidate {} · {} evidence records",
                    candidate.from,
                    candidate.evidence.len()
                )?;
            }
        }
        if graph.truncated {
            writeln!(w, "\nDepth limit reached; additional relationships remain:")?;
            for f in &graph.frontier {
                writeln!(
                    w,
                    "  {} · {:?} · {} edges",
                    f.id, f.direction, f.omitted_edges
                )?;
            }
        }
    } else if let Some(file) = &out.transcript_file {
        writeln!(w, "{}", file.path.display())?;
    } else if request.action == Action::Show {
        let s = &out.sessions[0];
        let style = Style::new(request);
        writeln!(
            w,
            "# {}\n\nSession: {}\nProject: {}\nCreated: {}\nLatest reply: {}\nSource: {} ({})\n",
            style.paint(&safe(&s.title, false), "1"),
            style.session_id(s),
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
        writeln!(
            w,
            "Projects: {} · Showing {}\n",
            out.total,
            out.projects.len()
        )?;
        for p in &out.projects {
            writeln!(
                w,
                "{}  ({} sessions)\n  Created {} · Replied {}\n  {}\n",
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
        list_header(&mut w, out.total, out.sessions.len(), request)?;
        for (i, s) in out.sessions.iter().enumerate() {
            list_session(&mut w, i, s, request)?;
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
            writeln!(
                w,
                "   Coverage: local user/assistant text · Native database"
            )?;
            return Ok(());
        }
        writeln!(
            w,
            "   Coverage: {} · Index source {} · Indexed {}",
            if s.scope == Scope::IndexedUserQueries {
                "indexed user queries (body not synced)"
            } else {
                "user/assistant text from synced indexed turns"
            },
            s.index_source.as_deref().unwrap_or("unknown"),
            s.indexed_at.as_deref().unwrap_or("unknown")
        )?;
        if let Some(at) = &s.synced_at {
            writeln!(w, "   Body snapshot: {at}")?;
        }
    }
    Ok(())
}

fn timings(out: &Output) {
    eprintln!(
        "Elapsed {} ms · Body reads {} ({:.1} ms) · Metadata reads {} ({:.1} ms) · Cache hits {} ({:.1} ms) · Other {:.1} ms",
        out.stats.elapsed_ms,
        out.stats.parsed_files,
        out.stats.body_read_ms,
        out.stats.header_files,
        out.stats.metadata_read_ms,
        out.stats.cached_files,
        out.stats.cache_read_ms,
        out.stats.other_ms
    );
}

fn warnings(out: &Output) {
    for warning in &out.warnings {
        eprintln!("Warning: {}", safe(warning, false));
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
            eprintln!("No matching sessions.");
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
            labels.push("Next page →".into());
        }
        if has_previous {
            labels.push("← Previous page".into());
        }
        labels.push("Back".into());
        let prompt = format!(
            "{} matches · {}–{} · ↑↓ Select, Enter View, Esc Back",
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
                .with_prompt("Press Enter to return to the list")
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
        bail!(
            "browse requires an interactive terminal; use recent / search / show --json in scripts"
        );
    }
    let theme = ColorfulTheme::default();
    eprintln!("session-finder · Local Codex / TRAE IDE / TRAE CLI sessions\n");
    loop {
        let choice = Select::with_theme(&theme)
            .default(0)
            .with_prompt("Find sessions")
            .items([
                "Recently created",
                "Latest replies",
                "Browse by project",
                "Search keywords",
                "Show by ID",
                "Show relationships by ID",
                "Quit",
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
                            "{} ({} sessions) · {}",
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
                    eprintln!("No matching projects.");
                    continue;
                }
                if let Some(index) = Select::with_theme(&theme)
                    .default(0)
                    .with_prompt("Select a project · Esc Back")
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
                    .with_prompt("Keywords (leave empty to go back)")
                    .allow_empty(true)
                    .interact_text()?;
                if request.query.trim().is_empty() {
                    continue;
                }
                choose_sessions(&request)
            }
            4 | 5 => {
                let id: String = Input::with_theme(&theme)
                    .with_prompt("UUID, prefix, or session link (leave empty to go back)")
                    .allow_empty(true)
                    .interact_text()?;
                if id.trim().is_empty() {
                    continue;
                }
                let Some(id) = crate::text::session_id(&id) else {
                    eprintln!("Invalid session ID.");
                    continue;
                };
                if choice == 5 {
                    let depth: u32 = Input::with_theme(&theme)
                        .with_prompt("Relationship depth (1–100)")
                        .default(1)
                        .validate_with(|v: &u32| {
                            if (1..=100).contains(v) {
                                Ok(())
                            } else {
                                Err("Enter a value from 1 to 100")
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
                    .with_prompt("Press Enter to return to the menu")
                    .allow_empty(true)
                    .interact_text()?;
                result
            }
            _ => return Ok(()),
        };
        if let Err(e) = result {
            eprintln!("Error: {}", safe(&e.to_string(), true));
        }
    }
}
