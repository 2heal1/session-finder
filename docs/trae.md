# TRAE IDE 原生接入

## 命令

```sh
# 本地模糊搜索；与 Codex 共用英文拼写容错、中文跳字匹配和 --exact
./session-finder search "thredline" --provider trae --sort relevance
./session-finder search "threadline" --provider all --sort relevance -n 10
./session-finder search "切换分支" --provider trae --sort relevance
./session-finder recent --provider trae --json

# 原生来源自动检测更新；以下命令强制重建派生索引
./session-finder search "threadline" --provider trae --refresh
./session-finder sync --provider trae --id <conversation_id>
# 不指定 ID 会重建所有会话的全文索引
./session-finder sync --provider trae

# 将选中的用户/助手对话写入临时 JSONL，stdout 只返回路径
./session-finder show --provider trae <conversation_id>
# 获取文件路径、格式、字节数与简要会话信息
./session-finder show --provider trae <conversation_id> --json

# 离线 JSONL 导出或旧脚本 JSON 快照转换，无需运行 IDE
./session-finder search "threadline" --provider trae --trae-index ./export.jsonl
```

查询来源默认为 `all`，省略 `--provider` 即自动合并三种来源。`trae` 专指 IDE，独立的 [TRAE CLI 来源](traecli.md) 使用 `traecli`。合并发生在项目过滤、时间过滤、评分、排序、分页前；ID 消歧使用 `(provider, id)`。一个来源不可用时，`all` 提示并返回可用来源；显式 `--refresh` 失败则报错。`show` 的 ID 前缀必须全局唯一，可用 `--provider` 消歧。`relations` 默认且仅支持 Codex；`sync` 仍需指定 `--provider trae`。

## 按需读取会话文件

TRAE IDE 的 `show` 只为选中会话创建一个文件，普通 stdout 只有其绝对路径，耗时和警告仍在 stderr。`--provider all` 最终选中 TRAE IDE 会话时也使用这一行为。Codex 和 TRAE CLI 的正文输出不变。搜索、最近会话和项目列表不会生成这些临时文件。

路径为 `<系统临时目录>/session-finder/trae/<来源哈希>/<完整会话ID>.jsonl`。临时根目录遵循系统规则（macOS / Linux 可通过 `TMPDIR` 指定）；不同数据库、离线索引或 ContextFS 安装使用不同来源目录。文件名使用完整 ID，即使命令只给了唯一前缀。

文件是 UTF-8 JSONL：第一行包含 `type=session_meta`、`schemaVersion=1` 和完整公共 `session` 元数据；其余行包含 `type=message` 及 `role`、`text`、`timestamp`、`phase`、`id`、可选 `turn_id`。保留转换器读取到的全部用户/助手文本，不截断长消息；正文范围与下文的转换边界一致。它是规范化的可读对话文件，不是原始数据库或 `--trae-index` 查询索引格式。

`show --json` 保留公共的 `schemaVersion`、`source`、`home`、`timeZone`、`warnings`、`stats`，返回简要 `session`（ID、provider、标题、范围、消息数和索引/同步时间）及 `file`：

```json
{
  "path": "/absolute/temp/session-finder/trae/<来源哈希>/<会话ID>.jsonl",
  "format": "jsonl",
  "bytes": 12345
}
```

其中 `bytes` 是实际 UTF-8 文件字节数；响应不包含 `messages` 或正文预览。完整元数据和正文从 `file.path` 按需读取即可。

每次 `show` 读取当前来源允许读取的对话快照；文件内容相同时保留原文件，标题或正文改变时原子替换。目录权限 0700，文件权限 0600。文件不会随命令退出删除，可由用户或系统临时目录清理；删除后再次 `show` 会重建。文件写入失败时命令报错，不输出正文。`--no-cache` 仍生成本次请求的对话文件，因为它是 `show` 的输出；它不改变 ContextFS 的源快照新鲜度规则。

## 标题与中文搜索

默认从原生数据库 `chat_session.session_title` 读取真实标题。列表、精确搜索、模糊搜索及 `show` 使用该标题；空标题回退到首条提问，再回退到 ID。JSON 的 `titleSource` 分别为 `metadata`、`first_user`、`id`。显式 ContextFS 来源仍只提供提问索引，搜索输出范围提示；原生来源不再显示“缺少 IDE 标题”的旧提示。

**已验证 TRAE CN 3.4.15 macOS arm64 原生数据库。** `search "适配原因" --provider trae --exact` 能命中「分析 react-router 版本适配原因」；`search "切换分支" --provider trae --exact --sort relevance` 的首项为「Git Worktree 切换分支冲突」。两者都是实际数据库标题，未使用手工元数据或目标关键词特判。

离线 JSONL 和 schema 1 查询快照的每个会话可附带可选的规范化 `metadata`，例如：

```json
{"conversation_id":"010000000000400080000001","sessions":[],"metadata":{"title":"会话标题","createdAt":"2026-11-04T00:00:00Z","cwd":"/tmp/example","repositoryUrl":null,"branch":null}}
```

`createdAt` 必须为 RFC 3339，统一转换成 UTC；其余缺失字段保持未知。项目路径和仓库信息进入公共项目解析与过滤流程。创建时间不会被当成最后提问或回复时间。重复会话行仍合并轮次，最新显式 `metadata` 替换旧元数据；没有 `metadata` 的行不会清除已读取的元数据。空白或 null 标题触发正常回退。

例如 IDE 标题包含“切换分支”，原始提问却是“我想切到主分支”。完整跳字匹配要求每个查询字都出现，过去会因缺少“换”字而漏掉。公共 Rust 评分现在对 4–64 字的纯中文词允许缺少一个中间字，同时要求首尾字和其余字符按序出现，匹配跨度不超过查询字数的三倍。评分顺序为子串、完整跳字、缺一字跳字；`--exact` 不启用跳字或缺字容错。该规则适用于所有来源，不使用同义词字典，也不能替代标题索引或语义搜索。查询“切到主分支”仍可直接匹配原文。

## 数据路径与转换边界

```text
Codex rollout JSONL / zstd ── Codex parser ──┐
TRAE SQLCipher database ── Native adapter ──┤
                                           ├─ Session + Message
TRAE ContextFS ── 源快照 ── TRAE converter ───┘
                                               │
                                      公共摘要 / 规范化文本
                                               │
                                      SQLite + zstd 派生索引
                                               │
                                      公共评分 / 过滤 / 分页
```

`src/trae_db.rs` 默认发现 `~/Library/Application Support/Trae CN/ModularData/ai-agent/database.db`，其次检查 `Trae`。可用 `--trae-db` 和 `--trae-library` 指定其他路径。数据库以 `SQLITE_OPEN_READ_ONLY`、`query_only` 和读取事务打开，包含实时 WAL，不采用会忽略 WAL 的 `immutable=1`。不启动应用、Node 或后台程序。

加密参数从已安装的 `libai_agent.dylib` 提取，先校验 Mach-O 构建 UUID，再按该构建的布局在内存中派生 SQLCipher 参数；临时材料使用 `Zeroizing` 清除。代码和缓存不含实际密钥。当前仅适配 TRAE CN 3.4.15 arm64；其他构建明确报错，不盲用固定偏移。非 macOS 仍可读取明文导出数据库、ContextFS 快照及 Codex 文件。

原生转换使用 `chat_session`、`project` / `session_project`、`worktree`、`chat_message` 及三类正文表。保留未删除消息（含压缩归档前的历史），排除已删除会话/消息。用户文本取 `general` 和追加输入；助手文本取 `thought` 可见进度，以及 `finish.summary`、`ReplyUser.message` 最终回复和 `chat.content`。忽略推理、工具参数/输出、隐藏 plan item、图片。空任务 `{}` 合法；未知正文类型明确报错。时间使用原始秒级字段和 `generated_at_ms`，不从 ID 或 mtime 推断；`assistant_sub_chat` 遵循公共子代理过滤，未推断父子关系。

显式 `--trae-ctx` 或 `--trae-index` 优先选择兼容快照来源；未发现原生数据库时也尝试已有 ContextFS 流程。`src/contextfs.rs` 使用 UnixStream 实现最小只读客户端，协议为 TTHeader + framed Thrift binary `ExecuteCommand`，调用 `icube.common.commands.tooling.readFile`。具备帧长、递归和超时限制。ContextFS 应用发现核验 Bundle ID 并排除 TRAE SOLO，实时读取核对所选路径与当前 Agent 应用一致。

`src/trae.rs` 转换：

- `context://history/conversations_metadata.jsonl` 的 `conversation_id` 是会话 ID。`sessions[].session_id` 是该会话的轮次 ID。重复会话记录合并，重复轮次更新，不生成父子关系。
- 索引轮次按 ID 稳定排列，避免 IDE 的 map 输出顺序变化使快照指纹失效；这是确定性的展示顺序，不作为时间证据。
- 每个轮次读取 `context://history/<conversation_id>/<turn_id>.jsonl`。支持字符串、字符串数组和 typed text 数组。仅提取用户/助手文本，用户注入上下文经过公共清洗函数。
- IDE 的 skills / collaboration / attachment 包装只保留其中的 `<user_input>`。内部审批请求按已验证的专用前缀识别，连同对应审批 JSON 回复排除；普通对话中相同 JSON 文本仍保留。
- 轮次内相同消息 ID 的修订采用最后版本；仅内容相同不足以去重。跨轮次保留消息和 `turn_id` 来源，避免删除合法重复提问。若源快照重复携带历史文本，正文也可能重复显示。
- `parser::summarize` 是公共转换边界。时间仅接受原始时间字段，不从 ObjectID、快照时间或文件 mtime 推断；项目和关系缺失时保持未知。查询索引不包含助手正文，`messageCount` / `replyCount` 为 null。

## 原生索引与 ContextFS 快照语义

原生来源每次核对 DB/WAL 的设备号、inode、大小和纳秒修改时间；数据不变时复用目录缓存。数据变化时重新读取会话元数据和消息修订号，只转换变化的会话。对仍在写入的秒级时间窗口，额外加入 DB/WAL 指纹，捕获同一秒内的流式更新。目录缓存仅在读取事务期间来源指纹稳定时发布。修改标题、项目路径、正文或删除会话都会使相应索引失效。`--refresh` 和 `sync` 强制重建正文索引；`show` 直接读取选中会话正文。

原生结果为 `scope=local_conversation`，完整元数据中的 `indexSource=native_database`。标题、全文、项目和时间进入公共过滤、评分、排序、分页管线。正文只覆盖当前数据库保存的可见文本，不包括图片/OCR、工具输出、推理或已删除内容。

以下规则仅用于显式选择的 ContextFS / 离线快照来源：

`search` 默认读取快照。`--refresh` 重新读取查询索引；它不会自动同步所有正文。`sync` 总是先刷新索引，再重新读取所选会话的全部索引轮次，因为同一个轮次 ID 的助手回复可能仍在变化。每个会话完整读取成功后才原子替换其正文快照；中途失败保留该会话的旧快照。批量同步中，之前已成功的其他会话仍会保存。

正文快照绑定完整轮次列表及索引提问的指纹，轮次或提问变化时退回提问搜索，重新 `sync` 后恢复正文搜索。标题、项目等元数据单独变化会使派生搜索缓存失效，但不会丢弃已同步正文；兼容已有 schema 2 正文指纹。同一轮次内容变化无法仅从索引检测，需要显式同步。`show --refresh` 更新查询索引并重新同步选中会话。普通 `show` 有有效正文快照时无需凭据。

结果的 `scope` 区分可搜索范围；列表使用 `--json --verbose` 可查看 `indexSource`、`indexedAt` 和 `syncedAt` 等完整元数据。默认 JSON 列表保留简要信息，搜索附带最多两段原文命中。`indexed_conversation` 只代表当前索引覆盖的轮次，不能保证全历史完整。TRAE 缺少时间时在时间排序末尾，时间范围筛选不匹配这些会话；建议关键词搜索用 `--sort relevance`。

## 缓存与权限

默认根目录复用 session-finder 的缓存目录：macOS 为 `~/Library/Caches/session-finder`，可用 `XDG_CACHE_HOME` 或 `--cache-dir` 覆盖。

- `trae-query-index/<SHA256(规范化 ctx-cli 路径)>.json`：兼容旧 Node 脚本的 schema 1 查询快照，保存会话、轮次、清洗后的提问、可选会话元数据及快照元数据。现有 ContextFS 实时读取不填充会话元数据。
- `trae-transcripts/<来源哈希>/<conversation_id>.json`：schema 2 的规范化用户/助手消息快照；转换规则升级后需重新同步旧正文。
- `index-v1-*.sqlite`：复用现有 SQLite + zstd 索引实现。来源及安装路径使用独立命名空间，Codex 和 TRAE 不会在缓存清理时互删。元数据转换使用 `trae-adapter-v3` 命名空间，不复用缺少标题来源字段的旧派生记录。
- `trae-native-catalog/<来源哈希>.json`：原生会话目录、规范化元数据和修订摘要。原生派生索引使用 `trae-native-v1` 命名空间，和 ContextFS 缓存分离。

新建私有目录权限 0700，快照文件 0600，采用同目录临时文件加 rename 原子替换。原生解密参数和 ContextFS 环境凭据只在内存中使用，不写入快照、索引或错误文本。

原生 `--no-cache` 禁用目录缓存和派生 SQLite 读写，直接读取数据库。ContextFS 的 `--no-cache` 关闭派生 SQLite 读写，仍读取源快照；显式刷新与同步会保存源快照。`--trae-index` 是只读离线来源，不支持刷新或同步。损坏的 ContextFS 查询快照需在 Agent 环境 `--refresh` 重建；损坏的正文快照可 `sync` 修复。

## 本机验证（2026-11-05）

原生数据库接入后：127 个未删除会话，默认排除 3 个子代理会话。移除 `CFS_TOKEN`、`CFS_TRANSPORT`、`CFS_IPC_NAME` 和 `ICUBE_APP_ROOT` 后，两个真实标题的精确搜索均成功；router 会话 `show` 返回 7 条用户提问、7 条可见进度及 7 条最终回复。首次全文转换约 2.5 秒。目录缓存优化后，7 次连续搜索的墙钟中位为 84.38 ms；数据稳定的 5 次全部命中 127 个会话缓存，墙钟 81–87 ms。前两次因当前会话仍在写入，重新核对目录并转换 1 个会话，耗时约 0.7–2.1 秒。

65 项 Rust 测试、17 项 Node 测试、fmt 和 clippy 通过；包含加密 fixture、只读/WAL、同秒流式更新、改名、删除、空任务、全文排除字段、项目/时间过滤、冷/热/无缓存一致性。1,200 个 Codex 合成日志、7 轮同机基线比较通过全部结果、读取量及耗时门槛，报告见 `bench-results/native-db/report.md`。

以下是原生数据库接入之前的 ContextFS 基线，范围及限制仅属于当时实现：

macOS arm64 release，真实 TRAE 索引 90 个会话，其中一个会话同步 8 个轮次、36 条用户/助手消息（8 条提问、28 条回复）。缓存查询移除 `CFS_TOKEN`、`CFS_TRANSPORT`、`CFS_IPC_NAME` 和 `ICUBE_APP_ROOT`，验证普通终端独立发现 Trae CN 并排除 TRAE SOLO。查询为 `thredline`，预热一次后取 7 次中位数，墙钟包含进程启动和 JSON 输出：

| 操作 | 墙钟耗时 | 查询内部耗时 |
| --- | ---: | ---: |
| Rust TRAE 缓存搜索 | 13.34 ms | 5 ms |
| 旧 Node 脚本的提问索引搜索 | 40.44 ms | 12 ms |
| Rust 实时刷新（单次） | 697.59 ms | 684 ms |
| Rust 8 个轮次正文同步（单次，包含索引刷新） | 2,185.15 ms | 1,739 ms |

两种 TRAE 搜索都命中同一个会话；Rust 此时还覆盖该会话已同步的正文，Node 只覆盖提问。离线 `show` 返回 36 条消息，未残留内部审批请求或回复；同步后再刷新索引仍可离线查看正文。联合搜索命中 17 个 Codex 会话和 1 个 TRAE 会话。耗时属于这次机器、数据量和服务状态，不代表通用上限。

初次接入时，55 项 Rust 测试、17 项 Node 测试、fmt 和 clippy 通过。Codex 的 1,200 会话、7 轮同机基线比较通过全部结果、读取量和耗时门槛；最近会话冷/热缓存中位 176.1 / 55.7 ms，原二进制 177.5 / 54.6 ms。

中文缺字容错修复后，57 项 Rust 测试、fmt 和 clippy 通过。移除 Agent 凭据后，原命令 `search "切换分支" --provider trae --sort relevance` 从原来的 0 个结果变为 1 个，命中包含“我想切到主分支”的目标会话。此次命中来自原文的通用容错匹配，未补造 IDE 标题；90 个缓存会话中仍有 89 个仅覆盖索引提问。真实数据上的 `thredline`（TRAE）和 `modren federation`（Codex）结果数量及当前页顺序与修复前一致。

元数据转换阶段新增 4 项回归测试，并扩充已有正文同步测试；61 项 Rust 测试、fmt 和 clippy 通过。使用合成元数据验证 Codex/TRAE 标题精确搜索及评分一致、冷/热/无缓存结果一致、改名与删除标题使缓存失效、标题变更保留 schema 2 正文、项目和创建时间过滤、缺失字段及重复记录处理。此阶段未完成本机加密数据库读取和真实标题验证。
