# 性能回归检查

`bench/budgets.json` 是性能门槛，CI 和本地共用。基准数据全部现场生成，显式使用 `--provider codex` 和临时 `--home`，不访问真实 Codex、TRAE IDE 或 TRAE CLI 数据。当前数据集有 1,200 个会话、每个 16 条消息、24 个项目，包含 zstd 压缩、归档、fork、spawn、SQLite 元数据和标题索引。

## 运行

```sh
sh build.sh check
npm test
sh build.sh
npm run bench

# 修改代码前保存一份 release 二进制，比较同机的两个版本
cp session-finder /tmp/session-finder-baseline
# 修改、重新构建后：
npm run bench -- --baseline /tmp/session-finder-baseline
```

需要 Node.js 24（使用内置 SQLite 和 zstd），无 npm 依赖。`--binary` 指定当前二进制，`--output` 指定报告目录，`--runs` 可调整本地采样次数（3–31）。CI 固定使用配置中的 7 次采样。

报告默认输出到 `bench-results/report.json` 和 `report.md`。JSON 保存环境、每次墙钟时间、查询时间、读取计数及结果摘要，Markdown 适合快速比较。CI 将报告写入 Summary 并上传 `performance-report` artifact，即使检查失败也保留已产生的数据。

## 门槛

- 每项使用 7 次采样的中位数；基线和当前版本交替运行，同机、同数据、独立缓存。
- 中位墙钟时间同时超过基线 **25%** 且增加 **10 ms** 以上，判定退化。
- 冷缓存上限 **1,500 ms**，热缓存上限 **750 ms**；单次进程超时 **15 s**。
- 每次执行检查正文读取、元数据读取和缓存命中计数；任何不符都失败。创建时间列表冷缓存只读 10 条正文，关系查询只读日志头；热缓存不重读正文或日志头。`show` 本身按需读正文，不使用摘要缓存，单独允许读取 1 条。
- 验证匹配总数、show 消息数、关系边数，跨样本及两个版本比较查询结果。避免通过少返回数据获得虚假的提速。

比较旧版二进制时，为旧 Codex 会话结果补齐新增字段的默认值 `provider=codex`、`scope=local_conversation`。支持列表版本 2 的二进制在基准中使用 `--json --verbose`，继续校验全部元数据；跨版本只忽略新增 `matches` 并统一列表版本号。当前二进制的冷/热结果仍比较包括片段在内的完整输出。该基准衡量完整元数据输出的耗时，默认精简 JSON 的体积需另行测量。

“冷缓存”指 session-finder 的空缓存，不是清空 OS 文件缓存。先预热可执行程序和 OS 页面，再对每轮使用新的工具缓存目录。墙钟时间包含 CLI 启动及 JSON 输出；`stats.elapsedMs` 只包含查询及工具缓存处理。终端中的 `耗时` 同样是查询时间，不包含等待交互输入。

PR 使用目标分支 base SHA，main push 使用前一个 SHA。基线用相同 Rust 工具链构建。如果最初的基线提交还没有 `Cargo.toml`，CI 会明确标为 bootstrap，仅执行绝对耗时和读取计数门槛；已有 Rust 基线的构建失败会直接失败，不会静默跳过。release tag 的比较基线在无 before SHA 时使用父提交。

预算变化需要连同原因和报告一起评审。若查询语义有意变化，应更新合成数据预期，并明确评估基线结果差异。CI 能报告失败；要阻止带失败检查的 PR 合并，仓库管理员还需在分支规则中将 `Tests and performance` 及 `Validate npm package` 设为必需检查。
