# session-finder

用 Rust 编写的本地 Codex / TRAE IDE / TRAE CLI 会话检索 CLI，支持统一模糊搜索、项目筛选、终端菜单、完整对话查看和 JSON 输出。Codex 另支持 fork / 子会话关系发现。npm 包名为 `session-finder`，首次发布准备中；Cargo 保持 `publish = false`。

省略 `--provider` 时，搜索、最近会话、项目列表、按 ID 查看和交互菜单默认合并 Codex、TRAE IDE、TRAE CLI，等同于 `--provider all`。需要限定来源时，使用 `--provider codex`、`--provider trae` 或 `--provider traecli`。

## 安装与验证

首次发布完成后可运行 `npm install -g session-finder`。npm 包内置 macOS 13+ / Linux glibc 2.35+ 的 arm64、x64 原生程序。当前也可以从源码执行 `sh build.sh`，直接使用生成的二进制。

```sh
sh build.sh check   # fmt、clippy、Rust 回归测试
npm test            # Node.js 24：性能门槛及发布保护测试，无 npm 依赖
npm run bench       # 先 sh build.sh；输出每个场景的冷/热缓存耗时
```

详见 [性能门槛与基线比较](docs/performance.md) 和 [npm CI、首次 OTP 发布及后续 OIDC 发布](docs/release.md)。

普通文本和交互菜单默认在 stderr 显示查询耗时、正文读取、元数据读取与缓存命中数。JSON 保留 `stats.elapsedMs`；加 `--timings` 可额外在 stderr 显示统计，stdout 始终是纯 JSON：

```sh
./session-finder recent --json --timings
# stderr 示例：耗时 34 ms · 正文读取 0 · 元数据读取 0 · 缓存命中 47
```

## TRAE IDE 原生搜索

```sh
./session-finder search "thredline" --provider trae --sort relevance
./session-finder search "threadline" --sort relevance --json
# 默认读取原生数据库；普通终端也可强制重建派生索引
./session-finder search "threadline" --provider trae --refresh
# 重建指定会话的用户/助手正文索引
./session-finder sync --provider trae --id <conversation_id>
# 仅生成选中会话的临时 JSONL，stdout 返回绝对路径
./session-finder show --provider trae <conversation_id>
# 显式选择兼容的 ContextFS 快照来源
./session-finder search "关键词" --provider trae --trae-ctx "/Applications/Trae CN.app/Contents/Resources/app/modules/ai-agent/bin/ctx-cli"
```

默认来源为 `all`，`trae` 指 TRAE IDE，`traecli` 指 TRAE CLI，三个来源统一排序和分页。某个来源不可用时提示并继续返回其余来源；显式 `--refresh` 失败仍报错。`relations` 省略来源时仍查询 Codex，`sync` 仍需指定 `--provider trae`。IDE 默认只读访问本机原生数据库，读取真实标题、创建时间、项目路径及用户/助手正文。各来源映射为统一 `Session` / `Message`，复用摘要、文本清洗、模糊评分和 SQLite + zstd 索引。

原生数据库目前已适配 **TRAE CN 3.4.15 macOS arm64**。先核验安装库的构建 UUID，再在内存中派生读取参数；不保存密钥，不依赖 `CFS_TOKEN`，不启动 IDE 或其后台程序。其他原生库版本会明确报错。自定义数据库或安装目录可用 `--trae-db`、`--trae-library`。

原生来源输出 `scope=local_conversation`，完整元数据中包含 `indexSource=native_database`，自动检测改名、消息更新和删除。正文排除工具输出、推理字段、图片内容；保留可见进度说明及最终回复。`--trae-index` 离线导出和 `--trae-ctx` ContextFS 快照仍可使用，后者仅刷新/同步时需要 Agent IPC。详见 [TRAE 数据接入、缓存和转换器](docs/trae.md)。

TRAE IDE 的 `show` 按需把选中会话写入系统临时目录中的 `session-finder/trae/<来源哈希>/<会话ID>.jsonl`，普通输出只有绝对路径；`--json` 返回简要 `session` 和 `file: {path, format, bytes}`，不内联消息正文。首行为会话元数据，随后每行是一条用户或助手消息。内容不变时复用文件，内容变化时原子更新；临时文件可随时删除，再次 `show` 会重建。`search` / `recent` 不生成这些导出文件。

## TRAE CLI 搜索

```sh
./session-finder recent --provider traecli
./session-finder search "关键词" --provider traecli --sort relevance
./session-finder show <UUID或前缀> --provider traecli
./session-finder browse --provider all
# 自定义 CLI 私有目录
./session-finder recent --provider traecli --traecli-home /path/to/cli
```

默认目录按 `--traecli-home`、`TRAECLI_HOME`、`TRAE_HOME/cli`、`~/.trae/cli` 的顺序选择。读取现存 rollout、`session_index.jsonl` 及 `state_*.sqlite` 元数据；支持旧消息格式、新版 `history_mutation`、可见回复和撤回轮次。标题优先读取原生名称，回退标题索引、数据库标题和首条提问。支持完整正文搜索、项目/时间筛选、归档压缩日志、独立缓存和 `--refresh` / `--no-cache`；不启动 TRAE CLI。

本机验证了 22 个现存 CLI 会话；两段新版对话与原生历史数据库的用户/助手文本及顺序一致，最终版本 7 次热查询中位约 15 ms。仅存元数据而缺失 rollout 的历史记录不纳入可读会话集合。`relations` 当前仅支持 Codex；续接 CLI 对话可用 `traecli resume <UUID>`。详见 [TRAE CLI 格式与验证范围](docs/traecli.md)。

## 直接使用

```sh
# 交互菜单：方向键选择、Enter 查看、Esc 返回
/Users/bytedance/personal/session-finder/session-finder

# 最近创建的会话
/Users/bytedance/personal/session-finder/session-finder recent --sort created

# 你之前使用的项目查询
/Users/bytedance/personal/session-finder/session-finder recent -p threadline --since 7d
```

在仓库目录内也可以使用 `./session-finder`：

```sh
./session-finder browse
./session-finder recent --sort replied -n 20
./session-finder recent --sort created -n 10 --offset 10
./session-finder recent -p modern --since 2026-11-01 --until 2026-11-04
./session-finder search "modren federation"
./session-finder search "路由导航" -p modern
./session-finder search "精确子串" --exact
./session-finder search "关键词" --sort relevance --json
./session-finder recent --json --verbose  # 完整列表元数据
./session-finder projects
./session-finder projects threadline --json
./session-finder show <UUID或唯一前缀>
./session-finder show "codex://threads/<UUID>" --json
./session-finder relations <UUID或唯一前缀> --depth 3 --json
./session-finder --id <UUID或前缀> --json
./session-finder --help
```

Codex 和 TRAE CLI 的 `show` 输出全部用户和助手文本，`--json` 保留内联 `messages`；可以通过 `./session-finder show <ID> | less -R` 翻阅长对话。TRAE IDE 的 `show` 返回临时 JSONL 路径，Agent 可按需读取。没有参数且连接终端时进入菜单；重定向或管道中默认列出最近回复的会话。交互列表支持上一页、下一页，按创建时间浏览时只读取当前页正文。

## Codex 查询规则

- 来源：`--home`、`CODEX_HOME` 或默认 `~/.codex`，递归读取 `sessions/` 和 `archived_sessions/` 的 `rollout-*.jsonl` / `.jsonl.zst`。压缩日志流式解压，不生成解压副本。
- 创建时间：优先使用最新兼容 `state_*.sqlite` 的 `threads.created_at_ms` / `created_at`，其次使用自身 `session_meta.timestamp`，最后回退到文件名。普通搜索和浏览只显示有本地日志的会话；关系查询也保留仅有数据库或引用记录的节点。
- 最新回复：取真实助手文本消息的时间，包含进度消息和最终回答。文件修改、工具活动、后续用户提问及数据库更新时间不算回复。无回复的会话排在最后；指定回复时间范围时不匹配。
- 分叉：按文件自身 UUID 选择 metadata，不被嵌入的父会话头或 `session_id` 别名覆盖。继承文本可以搜索和查看，但早于当前分叉创建时间的消息不算自己的回复和首问。数据库创建时间参与此判断。
- 项目：使用 cwd、记录中的 Git 地址及本地 `.git` / `commondir` 解析 worktree。数据库当前 cwd、仓库、分支和来源优先。完整项目名、仓库标识或路径优先于模糊匹配；这个优先级在隐藏子代理和时间过滤前确定。
- 搜索：Codex 检索标题及用户、助手文本；TRAE 检索已提供的标题元数据、索引提问和已同步正文。当前实时 ContextFS 索引不含 IDE 标题，缺少标题时列表回退到首条提问或 ID，JSON 的 `titleSource` 标明来源。不搜索工具输出和系统注入。多个词必须全部匹配；忽略大小写、全半角和标点，支持英文拼写错误、相邻字母交换和中文短距离跳字。4–64 字的纯中文词还允许缺少一个中间字，但首尾字、顺序和距离限制仍须满足，得分低于完整跳字匹配。`--exact` 关闭这些容错，仍保留规范化子串匹配。不支持语义或拼音搜索。
- 时间：`--since` 包含起点，`--until` ISO 时间不包含终点，纯日期包含当天；纯日期和终端显示使用系统时区，JSON 时间使用 UTC。相对时间支持 `7d`、`24h`、`30m`、`now`。时间范围使用 `--sort created/replied` 指定的时间轴。
- 标题：Codex 按最新 `session_index.jsonl` 标题、数据库标题、首条用户提问、UUID 依次回退。TRAE 按导出元数据标题、首条用户提问、会话 ID 回退。改标题无需重读正文。
- 子代理默认隐藏；`--include-subagents` 和按 ID 查询可以包含。普通 fork 保留。
- ID 支持完整 UUID、至少 8 位前缀和包含 UUID 的链接。`--id` 列出匹配项；`show` 要求唯一匹配，歧义时报错。
- 重复日志：同 ID 优先选择最新回复的副本，再比较消息数，并保留全部路径。相邻 event / response 双格式记录去重，同一 ID 的扩展消息保留较长版本；之后重复提问仍保留。
- `show` 不限制消息条数或单条正文长度。图片显示类型标记，工具原始记录仍在源文件中。工具不自动拼接父会话，不补齐日志中不存在的历史。
- 正在追加的不完整尾行暂时忽略，文件变化后重新读取；已完成的损坏行通过 warning 提示。创建时间查询若无法读取当前页正文，仍保留 metadata 并标记 `detailsAvailable: false`。

## 会话关系与 Threadline 接入

```sh
# 本会话的直接父会话、fork 和子代理；related 是 relations 的别名
./session-finder relations <ID>

# 沿父 → 子方向展开最多 3 层
./session-finder relations <ID> --direction children --depth 3

# 回溯父会话
./session-finder relations <ID> --direction parents --depth 10

# Threadline 读取结构化节点、关系与证据
./session-finder relations <ID> --depth 3 --json
```

`--direction` 默认 `both`，`--depth` 默认 1，范围 1–100。`both` 分别追溯祖先和后代，不沿父会话扩展到兄弟会话。自动包含子代理、不分页、不受默认 10 条上限影响，不接受项目或时间过滤。循环关系也会终止；达到深度限制时用 `truncated` 和 `frontier` 列出未展开的方向及边数。ID 前缀必须唯一；数据库节点及被关系引用的 ID 也参与匹配。

关系方向统一为父 → 子：

| kind | 含义 | 证据 |
| --- | --- | --- |
| `fork` | 从已有会话分叉 | 子会话自身 `session_meta.payload.forked_from_id` |
| `spawn` | 会话发起子代理 / 子会话 | `thread_spawn_edges`、`threads.source.subagent.thread_spawn/spawn.parent_thread_id`，或自身日志中的同类字段 |

每条边包含稳定 `id`（`kind:父UUID:子UUID`）、`from`、`to`、`kind`、`evidence[]`。每条证据标明 `kind`、本地 `path`、`field`，日志证据带 1 起始的 `line`，数据库证据带 `table`；关系表里的 `status` 原样保留，不作为过滤条件。相同关系的多个副本合并为一条边，并保留所有证据。

`spawn` 也包含有明确父 ID 的自动审查子会话。节点的 `threadSource: guardian_review` 可用于区别于普通子代理；Threadline 可按这个字段决定是否展示。

子代理继承父上下文时，记录里可能同时存在 `forked_from_id` 和 spawn 来源，因此同一对父子节点可以同时具有 `fork`、`spawn` 两条边。两种事实分别保留；统计会话数请按节点 ID 去重，不要直接使用边数。

同一孩子、同一种关系有多个父 ID 时，优先级是原生关系表 > 数据库 source > 自身日志。不同父 ID 的最高优先级相同则不建立确定边。`conflicts[]` 保留所有候选边及证据，`selectedParentId` 为最终采用的父 ID 或 `null`。冲突不会按文件修改时间猜测，也不会作为路径继续展开。

关系节点为独立的轻量结构：

- `id`、`title`、`createdAt`、`cwd`、`source`、`threadSource`、`nativeUrl`、`paths[]`。
- `inDatabase` 表示有 `threads` 记录；`metadataAvailable` 表示读到了自身日志头或数据库元数据。
- `logStatus`：`available`（至少一份日志的元数据前缀可读）、`unreadable` 或 `missing`。`available` 不保证全文无损；完整消息通过 `show <ID>` 按需读取。
- `parentStatus`：`linked`（至少一条确定父关系）、`conflicting`、`unknown`（已知是子代理 / agent 创建但缺父 ID，或没有足够元数据）、`not_recorded`（普通来源未记录父关系）。后两者都不能证明它是根会话。这个状态根据全量本地关系计算，可能存在本次方向或深度之外的父边。

JSON 顶层在公共字段之外包含 `rootId`、`depth`、`direction`、`nodes[]`、`edges[]`、`conflicts[]`、`truncated`、`frontier[]`、`coverage`。`coverage` 列出所用数据库、原生关系表是否可用、没有自身 metadata 的日志、读取失败日志和 `toolsScanned: false`。`truncated: false` 仅表示已展开所发现的关系，不代表历史完整。

当前发现范围是本地结构化元数据。部分 `threadSource: agent_created_thread` 记录只有“由 agent 创建”的标记，没有调用者 ID，工具会标为父来源未知。没有把聊天文本中提到的 ID、工具调用次数、相似标题、时间接近或同项目当作关系，也不解析工具调用输出来补推调用者。

为了反查 fork，首次关系查询会检查所有活动及归档日志，流式读取到属于文件自身 ID 的第一个 `session_meta` 后停止。若继承的父头在前，可能需要读取更长的前缀；如果自身头缺失，会检查至文件末尾并报告覆盖缺口。不会建立正文索引。独立 `relation_headers` 缓存只存选取后的元数据和证据位置，不存系统指令或工具内容；后续按文件指纹增量更新，数据库关系每次重读，支持 `--refresh` / `--no-cache`。

## 索引与性能设计

索引使用本工具独立的 SQLite 数据库。摘要和压缩后的规范化全文分表保存，按文件增量更新；普通 recent 不加载全文。最多使用 4 个线程并行解析候选日志。

1. `recent --sort created`：元数据筛选、排序后，只解析展示页正文。无可用数据库记录时读取日志头；有冲突的重复副本需比较正文。
2. `recent -p ...`：先确定项目及子代理范围，再读取候选会话摘要。首次需要扫描候选正文才能得到真实回复时间，不用创建日期提前排除可能最近回复的旧会话。
3. `search ... -p ...`：先筛选项目，再读取候选全文。首次全文索引仍需解析日志；后续复用压缩缓存。缓存同时保存规范化检索文本和清洗后的用户/助手原文；只为返回页生成命中片段，不额外读取日志。旧版仅含规范化文本的缓存会在首次全文读取时自动更新，摘要缓存仍可复用。
4. 文件大小、设备号、inode、纳秒级 mtime 和相关数据库元数据参与缓存校验。标题、项目解析每次更新；删除日志不再返回，缓存同时清理。项目查询和局部 `--refresh` 保留其他项目的缓存。

SQLite 使用事务和 WAL，支持多个 CLI 进程查询及更新不同项目。日志读取前后状态不一致时不缓存该次快照。缓存失效、损坏或不可写时回退到日志并提示。

macOS 默认缓存位于 `~/Library/Caches/session-finder/`，Linux 默认 `~/.cache/session-finder/`，也支持 `XDG_CACHE_HOME`。新缓存目录权限 `0700`、数据库文件 `0600`。缓存含会话文本；仅在本机保存。

```sh
./session-finder recent -p threadline --refresh
./session-finder search "关键词" --refresh
./session-finder recent --no-cache
./session-finder recent --cache-dir "/你选择的缓存目录"
./session-finder recent --home "/其他 Codex 数据目录"
```

Codex 数据库以只读方式打开；不读取 `auth.json`，不修改 Codex、Threadline、shell 配置，不发起模型调用或联网请求。构建阶段由 Cargo 下载开源依赖。

## JSON 接口

`recent` / `search`（包括默认列表、关键词简写和 `--id` 列表）使用 `schemaVersion: 2`；其余命令保持 `schemaVersion: 1`。公共字段包括 `source`、`home`、`timeZone`、`warnings`、`stats`。

- recent / search：`query`、`project`、`sort`、`total`、`sessions[]`。默认会话字段为 `id`、`provider`、`title`（最多 160 字符）、`project: {key, name}`、`createdAt`、`lastReplyAt`、`nativeUrl`、`scope`、`detailsAvailable`、`isSubagent`。
- 无关键词时增加 `preview`（末回复，缺失时使用末问，最多 160 字符）；有关键词时增加 `matchScore` 和 `matches[]`，替代末回复预览。每个会话最多两段、每段最多 180 字符；`field` 标明 `title`、`id`、`message` 或跨消息的 `messages`，单条消息附带 `role`。片段保留原文大小写、标点，折叠空白并移除控制字符，省略处显示 `…`。终端搜索同样展示命中片段。
- `recent / search --json --verbose` 恢复完整会话元数据：创建/回复/提问时间、完整项目、cwd、分支、来源、父会话/分叉 ID、首问/末问/末回复预览、消息数、匹配分、源文件和全部副本路径、压缩/归档状态等；搜索仍附带 `matches[]`。该选项仅用于 JSON 会话列表。依赖旧列表详细字段的脚本需加上 `--verbose`，并接受列表版本 2。
- show（Codex / TRAE CLI）：`session` 和完整 `messages[]`，消息含 `role`、`text`、`timestamp`、`phase`、`id`。
- show（TRAE IDE）：简要 `session` 与 `file: {path, format, bytes}`；完整元数据及消息在临时 JSONL 中，不内联 `messages[]`。
- projects：`total`、`projects[]`，包含项目 key/name/root/repository、会话数、最新创建/回复时间、cwd 列表。
- 完整元数据中的三项预览各最多 1,000 个 Unicode 字符。输出不含全文索引字段；终端输出移除控制字符，`show` JSON 与导出文件保留原始消息文本。
- `stats.mode` 为 metadata / summary / full / relations；`parsedFiles` 是正文解析数，`headerFiles` 是日志头扫描次数（可能含继承前缀和失败尝试），`cachedFiles` 是缓存读取命中次数。普通查询的 `candidateSessions/Files` 是项目和子代理过滤后的数量；relations 中是返回图的节点和日志数，`indexedSessions` 是返回节点数。

## 本地构建与检查

需要 Rust stable（支持 edition 2024）及本机 C 编译工具链。SQLite 和 Zstandard 编译进产物；运行 CLI 不需要 Node、Cargo、sqlite3 或 zstd 命令。

```sh
sh ./build.sh         # release 构建并生成仓库根目录的 ./session-finder
sh ./build.sh check   # 格式、Clippy、回归测试
```

`build.sh` 在 macOS 优先选择已安装的 Command Line Tools，仅设置本次构建进程的环境变量。没有修改系统 `xcode-select`。Cargo.lock 固定依赖版本；可执行文件和 target 目录已加入 `.gitignore`。

## 本机验证记录

2026-11-04，macOS arm64，Rust 1.97.1 release 产物约 5.3 MiB。44 项 Rust 回归测试（原有 25 项 + 关系查询 16 项 + 耗时输出 3 项）、`cargo fmt --check` 和 `cargo clippy --all-targets -- -D warnings` 均通过。另用伪终端实际验证了无参数菜单、方向键、翻页、完整正文、返回、项目选择、搜索和退出；关系菜单另验证了按 ID 查询、深度 2、审查来源标识、返回及退出。

Node 脚本测试覆盖性能门槛、读取量约束、归档内容、平台架构、临时包发布保护、重复发布处理以及 TRAE IDE 试用搜索。本机 npm 离线安装 smoke test 通过。1,200 个合成会话的 7 轮同机基线对比通过全部门槛；最近会话冷/热缓存中位约 171/53 ms，关系查询约 138/39 ms。以下真实数据测试记录保留作为早期能力验证，持续性能门槛以 `bench/budgets.json` 和 CI 报告为准。

关系查询的真实数据验证：

- 样例父会话 `01a13f03-d762-7770-811a-3a389b0517ab` 返回 84 个直接子会话：63 个普通子代理与原生数据库记录一致，另外 21 个为日志头明确记录父 ID 的 `guardian_review`。后者使用独立 JSONL 解码核对。
- 一条真实 fork 的父子 ID 和证据行号与原始压缩日志一致；一条 `agent_created_thread` 的调用者缺失，正确标记 `parentStatus: unknown`，仍可发现它自己的后代。
- 全部 1,829 个日志中有一个 0 字节空日志，`coverage.logsWithoutOwnMetadata` 明确列出；没有日志读取失败。
- 该样例空工具缓存查询为 168 ms，缓存后三次为 42.5 / 34.8 / 35.2 ms，中位数 35.2 ms。包含进程启动与 JSON 输出；系统文件缓存未清空。未建立正文索引。
- 新旧 Rust 可执行文件的两组 recent 查询总数、ID 顺序一致。缓存后创建时间查询中位数为旧版 27.7 / 新版 32.9 ms，`threadline --since 7d` 为旧版 29.1 / 新版 32.6 ms，各测三次。

以下为关系功能加入前对本机 1,829 个日志文件与 Node 1.2.0 的对照记录：

- 全部 313 个默认可见会话的排序、创建/回复/提问时间、消息数量、来源、标题、cwd、分支和项目字段一致。
- 六组查询的总数、前 10 条结果及会话字段一致；`threadline` 项目统计为 17 个可见会话，最近 7 天回复查询返回 7 个。
- 一条真实会话的完整 `messages[]` 与 Node 输出一致；回归测试另覆盖超过 50 条消息、单条超过 10,000 字符的正文。
- 两个旧版提示 JSON 损坏的压缩日志，经完整解压验证每行都是有效 JSON；旧版流式读取结果与完整解压不同，Rust 读取正常。真正的损坏行和正在追加的尾行已通过独立用例验证。

以下耗时包含进程启动和 JSON 输出。空缓存没有清除操作系统文件缓存；缓存后为 3 次运行中位数。除创建时间查询外，空缓存列为一次测量。

| 查询 | Rust 空工具缓存 | Rust 缓存后 | Node 空工具缓存 | Node 缓存后 |
| --- | ---: | ---: | ---: | ---: |
| `recent --sort created` | 46 ms¹ | 43 ms | 143 ms¹ | 99 ms |
| `recent -p threadline --since 7d` | 58 ms | 42 ms | 138 ms | 100 ms |
| `recent --sort replied` | 849 ms | 46 ms | 7,311 ms | 138 ms |
| `search threadline -p threadline` | 64 ms | 46 ms | 162 ms | 120 ms |
| `search "modren federation" -p modern` | 180 ms | 50 ms | 775 ms | 147 ms |
| `search federation` | 1,095 ms | 83 ms | 7,965 ms | 466 ms |

¹ 创建时间查询复测时各使用 3 个独立空缓存，表中为中位数。Rust 三次为 651 / 46 / 46 ms；此前首次测量为 851 ms，因此第一次执行可能明显慢于后续执行，不能保证每次都是 46 ms。各次均只解析展示页的 10 个日志，没有建立全库正文索引。首次查询耗时随系统缓存和会话长度变化。
