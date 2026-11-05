# npm 发布

包名 **`session-finder`**，版本同时由根目录 `package.json` 和 `Cargo.toml` 管理。根 package 标为 `private: true`，仅负责开发脚本。发布脚本生成单独的公开 npm tarball；Cargo 仍禁止发布。

## 包结构和支持平台

单个 npm 包包含 `bin/session-finder` 和四份原生二进制。启动器用 POSIX shell 选择平台后直接 `exec` Rust 程序，不启动 Node，不需要安装脚本或二次下载。

| 目标 | 构建 runner | 运行要求 |
| --- | --- | --- |
| darwin-arm64 | macos-15 | macOS 13+，Apple Silicon |
| darwin-x64 | macos-15-intel | macOS 13+，Intel |
| linux-arm64 | ubuntu-22.04-arm | arm64，glibc 2.35+ |
| linux-x64 | ubuntu-22.04 | x64，glibc 2.35+ |

不支持 Windows 或 Alpine/musl。所有平台使用 Rust 1.97.1；CI 脚本使用 Node 24.18.1，OIDC 发布使用 npm 11.16.0。发布包没有 JavaScript 依赖。源代码授权许可证目前尚未声明。

## CI

`.github/workflows/ci.yml` 在 PR 和 main push 上运行，也被 release workflow 复用：

1. Rust fmt / clippy / 全部功能测试；Node 性能门槛与发布校验测试。
2. Release 构建、同机基线性能比较、读取计数和结果一致性检查。
3. 四个平台各自构建和测试，用本平台临时 npm 包执行离线安装 smoke test。
4. 汇总四份程序后生成最终 tarball，检查文件白名单、架构、执行权限、版本、无安装脚本及依赖，再安装验证。

Artifacts：`native-*`、`performance-report`、`npm-package`。临时单平台包带 `private: true`，不能被发布脚本接受。最终包缺任何一个平台都会失败。

## 本机预检

```sh
sh build.sh check
npm test
sh build.sh

# 以当前 Apple Silicon 主机为例；Linux/x64 修改对应目录名
mkdir -p dist/native/darwin-arm64
cp session-finder dist/native/darwin-arm64/session-finder
npm run release:pack -- --local --binaries dist/native --output dist/smoke
npm run release:smoke -- --local --archive dist/smoke/session-finder-0.1.0.tgz
```

完整包需从四平台 CI 获得所有产物；将它们按 `dist/native/<platform>/session-finder` 放置：

```sh
npm run release:pack -- --binaries dist/native --output dist/npm
npm run release:validate -- --archive dist/npm/session-finder-0.1.0.tgz
npm run release:smoke -- --archive dist/npm/session-finder-0.1.0.tgz
```

安装检查使用临时目录、离线 npm 安装，覆盖含空格路径、npm 全局软链接和相对软链接、版本、帮助、错误退出码，以及真实 JSON 查询和 stderr 耗时。所有输入均为测试数据。

## 首次发布与 OIDC

`session-finder` 首次公开发布前，npm 上还没有包，无法预先配置 trusted publisher。首次发布按下面顺序进行：

1. 提交并推送这些文件后，在 Actions 手动运行 `Release session-finder`，version 为 `0.1.0`、publish 为 **false**。它只做完整检查和打包。
2. 下载该次运行的 **npm-package** artifact，在同一提交的源码下执行 `release:validate` 和 `release:smoke`。首次必须使用四平台完整包，不能发布本机临时包。
3. 确认使用正确 npm 账号并准备真正发布时，再请求一次性 OTP：

```sh
npm whoami --registry https://registry.npmjs.org
npm run release:publish -- --archive /absolute/path/session-finder-0.1.0.tgz --otp <当次OTP>
```

4. 包存在后，为此包配置 GitHub trusted publisher：

```sh
npm trust github session-finder --repo 2heal1/session-finder --file release.yml --allow-publish --yes
```

若 npm 要求登录或证明本人操作，到这一步再完成认证。OTP 仅用于当次命令，不存储在文件或 GitHub secrets。若需要浏览器认证，可以使用账号已有的浏览器 profile。

## 后续发版

1. 更新 `Cargo.toml`、`Cargo.lock` 内当前包版本，以及 `package.json`；运行测试、性能检查并合并。
2. 推送与版本一致的 `vX.Y.Z` tag。`release.yml` 先执行完整 CI，再发布该次构建的 tarball。
3. 手动恢复发布也必须选中对应 tag，并勾选 publish；默认不发布。在 main 上的手动运行只允许构建预检。

OIDC job 位于 GitHub 托管 runner，只有 publish job 获得 `id-token: write`。不配置 `NPM_TOKEN` 或 `NODE_AUTH_TOKEN`。公开仓库使用 trusted publishing 时，npm 自动附带 provenance。

发布脚本先校验和安装检查，再查询目标版本：若该版本的 `dist.integrity` 与当前 tarball 完全一致则跳过；若版本已存在但内容不同则失败并要求升版；网络、认证错误不会当作“版本不存在”。发布后再次校验 npm 上的完整性摘要。

首次本地 OTP 发布的版本通常没有 GitHub Actions provenance；后续 OIDC 版本会有。这里不创建 GitHub Release，npm tarball 和完整性摘要保存在 Actions artifacts 中。
