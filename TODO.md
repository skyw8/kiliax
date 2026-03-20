
## MVP
cli
tui

skills、tools(read,write,shell)、mcp
slash command

权限管理
- shell allowlist
- 限制workspace目录


OpenAI-compatible tool calling
ReAct循环
长期记忆AGENTS.md

provider&models
- 支持openai compatible


持久化
- 会话消息先用jsonl文件存
- 日志用log文件存

## todo

配置读取
- 从./killiax.yaml 或./.killiax/killiax.yaml 或 ~/.killiax/killiax.yaml读取配置，优先级从高到低
- provider(base_url,apikey)以及所属的models，后续使用openai compatible api调用


实现agents模块
不同的agents有不同的工具和权限，以及提示词
plan agent 没有写权限，只能读文件，并且只能执行部分命令
build agent可读可写、可执行

工具模块
skills、tools(read,write,shell)、mcp

TUI
- 消息流式输出 + 底部固定输入行
- 不提供额外的滚动条，用户使用终端自带的滚动
- 使用ratatui+Crossterm，参考/home/skywo/github/codex/codex-rs/tui，UI可以借鉴复制
- 先实现对话界面，代码高亮，markdown语法高亮