# 飞书 GitHub Issue / PR 通知与派任务机器人

接收某个 GitHub organization 下多个仓库的 webhook，把 Issue / PR 动态推送到飞书：

广播型通知（下列除 review_requested 外）发给配置的 `notify_id`（默认按 `union_id` **私聊**；也可配成群 `chat_id`）。

- **PR 创建（opened）** → 发通知卡片。
- **请求 review（review_requested）** → 按被请求人的 GitHub 用户名查绑定关系，给对应飞书人**建任务**并**私聊发卡片**；未绑定则通知管理员去引导绑定。
- **PR 合并/关闭（closed）** → 更新卡片状态、把相关 review 任务标记完成。
- **Issue 新建/关闭/重开（issues）** → 发通知卡片。
- **Issue 新评论（issue_comment）** → 发评论卡片（PR 上的评论会跳过，避免与 PR 通知重复）。

reviewer 映射靠**用户自助绑定**：私聊机器人 → 收到一张卡片 → 填 GitHub 用户名提交 → 存入飞书多维表格。

**纯单向提醒**：机器人不回写 GitHub（不代为 approve/merge）。

## 架构

单个 Rust 二进制（tokio + axum + reqwest），两个 HTTP 入口：

- `POST /webhook/github` —— 收 GitHub webhook（HMAC-SHA256 校验 `X-Hub-Signature-256`）。
- `POST /webhook/feishu` —— 收飞书事件订阅 / 卡片回调（URL 验证、绑定卡片提交、私聊消息）。
- `GET /health` —— 健康检查。

存储用飞书多维表格 Base（两张表），无需自建数据库。

## 部署前置准备

### 1. 飞书自建应用

在[飞书开放平台](https://open.feishu.cn/)建一个企业自建应用，开通权限（scopes）：

| 能力 | 权限 |
|---|---|
| 发消息/卡片 | `im:message`（发送单聊、群消息） |
| 建/改任务 | `task:task`（读写任务） |
| 读写多维表格 | `bitable:app`（或对应记录读写权限） |
| 接收私聊消息 | 订阅事件 `im.message.receive_v1` |

- 记下 **App ID / App Secret**。
- 「事件与回调」→ 配置**请求地址** `https://你的域名/webhook/feishu`，记下 **Verification Token**；若开启加密，记下 **Encrypt Key**。
- 订阅事件：`im.message.receive_v1`（私聊触发绑定卡片）。卡片回调（`card.action.trigger`）走同一请求地址，无需额外订阅。
- 通知目标：默认按 **union_id 私聊**给管理员（填 `notify_id`）。要发到群则把 `notify_id_type` 改成 `chat_id`、`notify_id` 填群 `oc_xxx`，并把应用加入该群。

### 2. 多维表格 Base

新建一个多维表格，建两张表，**列名必须与下方完全一致**：

**表1「绑定映射」**

| 列名 | 类型 |
|---|---|
| `GitHub用户名` | 文本 |
| `飞书open_id` | 文本 |
| `绑定时间` | 日期 |

**表2「PR跟踪」**

| 列名 | 类型 |
|---|---|
| `PR键` | 文本（复合键 `org/repo#号`）|
| `仓库` | 文本 |
| `PR号` | 数字 |
| `标题` | 文本 |
| `群message_id` | 文本 |
| `reviewer任务` | 文本（存 JSON）|
| `状态` | 文本 |

记下多维表格 **app_token** 与两张表的 **table_id**。把机器人应用加为该多维表格的协作者（可编辑）。

### 3. GitHub webhook

在 org（或各仓库）Settings → Webhooks 新建：

- Payload URL：`https://你的域名/webhook/github`
- Content type：`application/json`
- Secret：自设一个随机串（填进配置 `webhook_secret`）
- 事件：勾选 **Pull requests**、**Issues**、**Issue comments**

## 配置与运行

```bash
cp config.example.toml config.toml
# 编辑 config.toml 填入非敏感值；敏感值建议用环境变量注入
cargo build --release

# 敏感值走环境变量（例如用 1Password：op run --env-file=... -- ...）
export FEISHU_APP_SECRET=...
export FEISHU_VERIFICATION_TOKEN=...
export FEISHU_ENCRYPT_KEY=...        # 未开加密可不设
export GITHUB_WEBHOOK_SECRET=...

./target/release/feishu-github-bot config.toml
```

配置文件路径也可用第一个命令行参数或环境变量 `FGB_CONFIG` 指定，默认 `./config.toml`。

### systemd（VPS 常驻）

```ini
# /etc/systemd/system/feishu-github-bot.service
[Unit]
Description=Feishu GitHub PR Bot
After=network-online.target

[Service]
WorkingDirectory=/opt/feishu-github-bot
ExecStart=/opt/feishu-github-bot/feishu-github-bot /opt/feishu-github-bot/config.toml
EnvironmentFile=/opt/feishu-github-bot/secrets.env
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

反向代理（Nginx/Caddy）把 `https://你的域名/webhook/*` 转发到本服务监听端口即可。

## 使用

1. 成员私聊机器人任意消息 → 收到绑定卡片 → 填自己的 GitHub 用户名 → 提交绑定。
2. 之后在该 org 的仓库开 PR / Issue → 收到通知卡片；给已绑定的人请求 review → 该人收到飞书任务 + 私聊卡片；PR 合并/关闭 → 卡片状态更新、任务自动完成。

## 测试

```bash
cargo test          # 单元测试：签名校验、事件归类
```

本地联调：见 `config.example.toml` 起服务后，用 GitHub webhook 页的 "Redeliver"，或用签名过的 fixture `curl` 打 `/webhook/github`；飞书侧点绑定卡片验证多维表格写入。
