# TRAE CLI 本地会话

`--provider traecli` 独立于 TRAE IDE 的 `--provider trae`。不调用 CLI 可执行文件，不依赖 IDE IPC，不读取认证文件。

```sh
./session-finder recent --provider traecli --sort created
./session-finder search "正文关键词" --provider traecli --exact
./session-finder recent --provider traecli -p my-project --since 7d
./session-finder show <UUID或至少8位前缀> --provider traecli --json
./session-finder projects --provider traecli
./session-finder browse --provider all
```

## 数据源

CLI 私有目录依次取：

1. `--traecli-home /path/to/cli`
2. 环境变量 `TRAECLI_HOME`
3. 环境变量 `TRAE_HOME` 下的 `cli/`
4. `~/.trae/cli`

`--home` / `CODEX_HOME` 仍只选择 Codex 来源。两者可与 `--provider all` 同时使用。

扫描私有目录的 `sessions/` 和 `archived_sessions/` 中 `rollout-*.jsonl` / `rollout-*.jsonl.zst`。元数据来自最新兼容的 `state_*.sqlite`（只读连接，包含 WAL），名称补充来自 `session_index.jsonl`。标题优先级为 `threads.name` → 标题索引 → `threads.title` → 首条用户消息 → UUID。创建时间、cwd、仓库、分支、子代理来源来自结构化元数据；回复时间来自可见助手消息。

只把磁盘上现存的 rollout 纳入会话集合。数据库或标题索引中的孤立记录无法保证正文可恢复，因此不作为完整会话返回。不会从 UUID、文件修改时间推算回复时间。

## 正文转换

复用 Codex 的流式 JSONL / zstd 读取、消息清洗、摘要和全文索引；额外处理 CLI 的记录：

- 旧版 `response_item`、`event_msg`、`message`：保留用户/助手可见文本，合并事件镜像，保留不同轮次的重复提问。
- `history_mutation` v1 `append`：读取 `items` 的用户/助手文本；有 `content_item_kinds` 时按 `user.*` 标记筛出用户内容。
- `display_completions` 和 `item_completed` 的 `AgentMessage`：读取可见回复，保留 `phase`、消息 ID、轮次 ID；优先采用可见回复，避免与内部消息重复。
- `history_mutation` 的 `rollback`：按目标 commit 截断已撤回记录；旧版 `thread_rolled_back` 按实际用户轮次回退，忽略空的控制轮次。
- 工具调用/结果、推理记录、开发者指令和注入环境不会作为对话全文索引。图片只有已有解析器的文字标记，不分析图片内容。
- 未知 mutation 版本或操作会提示并跳过该会话，避免把已知部分误报为完整正文。损坏的完整 JSON 行会计数并提示；正在写入的末尾不完整行在下次读取时重试。

输出 `provider=traecli`、`scope=local_conversation`，完整元数据中的 `indexSource=local_rollout`。列表默认 JSON 精简展示，可用 `--json --verbose` 查看完整元数据；搜索附带最多两段原文命中。没有已验证的 CLI 深链接，`nativeUrl` 留空。用户可执行 `traecli resume <UUID>` 续接；session-finder 不自动启动或恢复会话。

## 搜索与缓存

省略 `--provider` 等同于 `--provider all`，三个来源在共享查询引擎中统一进行项目过滤、时间过滤、模糊评分、排序和分页。默认 `show` 遇到跨来源相同 UUID 或非唯一前缀会要求指定来源。内存记录按 `(provider, path)` 隔离，即使显式把 Codex 和 CLI 配到同一目录也不会混用解析结果。`relations` 默认且仅支持 Codex。

CLI 使用独立的 SQLite + zstd 派生缓存命名空间。文件设备号、inode、长度、纳秒修改时间及影响正文摘要的元数据共同判定是否重读。名称在每次查询时装饰，改名无需重新解析正文；追加、替换和删除日志会使缓存及时失效。按 ID 刷新不会清掉其他会话缓存。

`--refresh` 强制重读本次涉及的日志；`--no-cache` 不读写派生缓存。`sync` 是 IDE 专用操作，CLI 无需同步到中间快照。CLI 当前在冷查询时读取所选 ID 范围的全部日志，随后共享引擎进行项目和分页过滤；热查询复用全文索引。

## 验证（2026-11-05，macOS arm64）

- 本机现存 22 个 rollout 会话，全部可解析，无警告。元数据库有 43 条记录，其中 21 条所指日志已不在磁盘；这些不计入上述 22 个。
- 两个新版 paginated 会话分别返回 2 条（1 用户、1 助手）和 5 条（3 用户、2 助手）消息；与 `thread_history_1.sqlite` 的可见用户/助手投影逐条文本、顺序完全一致。该数据库用于此次交叉验证，不是运行时依赖。
- 一次首次运行含进程启动约 444 ms，内部查询约 73 ms；最终版本 7 次热查询中位 14.76 ms。这里是 22 个本机会话的测量，不代表任意数据规模。
- 三来源联合查询返回 460 个默认可见会话：314 Codex、124 IDE、22 CLI，无警告。
- 全部 76 项 Rust 测试及 17 项 Node 测试通过。Rust 回归覆盖新旧格式、真实标题、WAL 改名、超出预览长度的全文搜索、撤回、重复消息、压缩归档、无缓存、追加/删除、过滤分页和跨来源同 ID / 同路径隔离。两段新版真实正文另与原生数据库交叉验证。
- Codex 的 1,200 个合成会话、7 轮同机基线性能检查见 `bench-results/traecli/report.md`。
