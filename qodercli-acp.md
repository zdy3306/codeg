> ## Documentation Index
> Fetch the complete documentation index at: https://docs.qoder.com/llms.txt
> Use this file to discover all available pages before exploring further.

# ACP

# **ACP 是什么**

ACP 协议是一种客户端与 Agent 之间的协议，可以用于 CLI 与各种编辑器集成，详见：[Agent Client Protocol](https://agentclientprotocol.com/overview/introduction)，Qoder CLI 实现了该协议标准, 通过该特性 Qoder CLI 可以被集成到任何一种实现了 ACP 协议的客户端中。

# **功能特性**

## **运行模式**

支持两种运行模式

* **默认模式**：等同于 CLI 的默认启动模式，按照默认配置的权限设置运行
* **Bypass Permissions** 模式：等同于 CLI 的 `--yolo` 模式，跳过权限检查，自动执行工具等

## **斜杠命令**

目前支持命令的列表如下，命令功能与 CLI 中对应命令功能相同

* `/init`：执行项目理解，生成 `AGENTS.md` 记忆文件
* `/memory`：显示或刷新记忆信息
* `/about`：显示版本信息
* `/help`：显示可用 ACP 命令

## **其他特性**

| **特性**          | **支持** | **说明**                                          |
| :-------------- | :----- | :---------------------------------------------- |
| 内置工具            | ✅      | 提供 CLI 中相同的内置工具                                 |
| Subagent        | ✅      | 提供 CLI 中相同的 Subagent 能力                         |
| MCP Server      | ✅      | 提供 CLI 中相同的 Stdio、SSE、Streamable HTTP 类型 MCP 支持 |
| 权限配置            | ✅      | 提供 CLI 中相同的权限配置能力                               |
| 上下文压缩           | ✅      | 提供 CLI 中相同的上下文压缩机制                              |
| 多模态             | ✅      | 支持图像                                            |
| 文件操作 / Terminal | ✅      | 通过 ACP 协议使用 IDE 侧提供的能力                          |

# **启动方式**

在启动之前请确保 Qoder CLI 已经安装，安装方式详见[Qoder CLI 快速上手](https://docs.qoder.com/zh/cli/quick-start)，目前支持的操作系统和 CPU 架构如下：

* 支持的操作系统：macOS、Linux、Windows
* 支持的 CPU 架构：arm64、amd64（Windows arm64 架构暂时不支持）

## **启动 ACP 服务器**

如果你有 ACP 客户端的开发场景，并期望通过 Qoder CLI 来实现 Agent Server，可以直接通过命令来启动 CLI。只需在启动 Qoder CLI 的时候，传递`--acp`的参数即可，CLI 会以 ACP 服务器的形式进行启动，ACP 客户端可以使用标准输入输出与该服务器进行通信。

```
qodercli --acp
```

## **在 Zed IDE 中启动**

Qoder CLI 与 Zed IDE 的集成只需在 Zed 配置文件中添加如下扩展配置，在 Zed IDE 中添加 Qoder CLI 支持，配置完成后创建 Thread 时即可选择 Qoder CLI。

* macOS / Linux 平台配置

```
{
   ...
   "agent_servers": {
      "Qoder CLI": {
          "type": "custom",
          "command": "qodercli",
          "args": ["--acp"]
      }
   }
}
```

* Windows 平台配置

```
{
   ...
   "agent_servers": {
      "Qoder CLI": {
          "type": "custom",
          "command": "~\\AppData\\Roaming\\npm\\qodercli.cmd",
          "args": ["--acp"]
      }
   }
}
```

注意在Zed版本号为0.215.2及之前版本里，type不需要配置。

不同操作系统下，Zed IDE 的配置文件路径如下：

* macOS：`~/.config/zed/settings.json`
* Linux：`~/.config/zed/settings.json`
* Windows：`~\AppData\Roaming\Zed\settings.json`

# **登录与使用**

ACP 客户端使用 Qoder CLI 相同的登录状态，目前需要通过 Qoder CLI 进行登录。如果你已经登录并且使用过 Qoder CLI，无需再次登录即可正常使用 ACP 客户端。

## **通过 Qoder CLI 登录**

如果你从未登录过 Qoder CLI ，请在终端中输入如下命令打开登录界面

```
qodercli login
```

Qoder CLI 会启动浏览器登录流程，打印登录 URL，并在可用时自动打开浏览器。请按照终端提示完成认证。

## **通过环境变量登录**

Qoder CLI 启动时支持检测`QODER_PERSONAL_ACCESS_TOKEN`环境变量完成身份验证，因此 ACP 客户端可以通过配置该环境变量来让 Qoder CLI 自动登录，下面是 Zed IDE 中添加 Qoder Access Token 环境变量的示例配置。

```
{
   ...
   "agent_servers": {
      "Qoder CLI": {
          "env": {
              "QODER_PERSONAL_ACCESS_TOKEN": "your_personal_access_token_here"
          },
          "command": "qodercli",
          "args": ["--acp"]
      }
   }
}
```

你可以在此页面获取 Personal Access Token：[https://qoder.com/account/integrations](https://qoder.com/account/integrations)
