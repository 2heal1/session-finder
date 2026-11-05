# TRAE IDE 会话搜索试用

`trae-search.mjs` 模糊搜索 TRAE IDE（provider `trae`）索引中的用户提问，并按会话合并命中的轮次。`traecli` 是另一个 provider。本脚本仍是独立原型，尚未接入正式 CLI 或 npm 包。

```sh
node scripts/trae-search.mjs "thredline"
node scripts/trae-search.mjs "路由导航" -n 5
node scripts/trae-search.mjs "threadline" --exact --json
```

## 普通终端与索引刷新

`ctx-cli` 是 ContextFS 客户端，需要 TRAE Agent 注入的 `CFS_TOKEN` 和 IPC 配置。仅找到应用里的 `ctx-cli` 文件，或打开 IDE 内置终端，不代表拥有这些凭据。

- 在具备 ContextFS 环境的 TRAE Agent 中运行时，脚本读取当前索引并更新本地缓存。
- 在普通终端中运行时，脚本搜索已有缓存，输出标明「本地缓存」和更新时间。
- 首次使用没有缓存时，脚本提示先让 TRAE Agent 刷新。
- `--refresh` 强制要求实时读取；缺少凭据、读取失败或索引无效时会报错，不会返回旧缓存结果。

让 TRAE Agent 执行下面的命令可更新索引；这条命令不能让普通终端自行获得凭据：

```sh
node scripts/trae-search.mjs "thredline" --refresh
```

普通终端的后续搜索不会自动获取新会话。要搜索最近新增的问题，需先从 Agent 环境刷新。无需复制或保存 `CFS_TOKEN`。

## 缓存与范围

macOS 默认缓存目录是 `~/Library/Caches/session-finder/trae-query-index`。`--cache-dir /path` 可指定一个专用目录；脚本将目录权限设为 `0700`，缓存文件权限设为 `0600`。缓存按读取器的规范路径区分，原子替换。

缓存保存会话 ID、轮次 ID、用户提问、读取器路径、版本及读取时间；不保存 ContextFS 环境凭据。提问正文会以明文保存在本机。JSON 输出中的 `indexSource` 是 `live` 或 `cache`，`indexedAt` 是本脚本读取索引的时间，`cacheAgeMs` 是距该时间的毫秒数。

脚本会识别 TRAE IDE 与 TRAE CN 的应用身份，也可通过 `--ctx /path/to/ctx-cli` 明确选择应用。指定应用与当前 Agent 所属应用不一致时，不使用当前凭据刷新它的缓存。

此索引不保证覆盖全部历史。脚本不搜索助手正文，也没有会话创建时间、最新回复时间、项目路径或分叉关系。结果中的 `context://history/...` 路径用于 ContextFS 读取，普通终端不能凭缓存直接打开会话全文。
