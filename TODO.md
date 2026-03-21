
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
- 快捷键：↑/↓ 切换历史 prompt，Ctrl+C 清空当前输入，Ctrl+D/Esc 退出

对话框修改
- TUI启动时，先有一个小信息栏显示版本号，使用模型，当前文件夹，信息栏之后是对话框
- TUI启动时，对话框不要固定在最下方，一开始对话框应该在信息栏之后，随着对话进行，对话框一直自动下推
- 对话框UI仿照codex，并且添加自动换行的逻辑，换行时对话框跟随变大
- 以上设计均参考codex的TUI实现，/home/skywo/github/codex/codex-rs/tui

关于工具调用，thinking内容的折叠
- 默认折叠这些内容
- 使用ctrl+o展开这些内容，能够查看
- 省略User Assistant显示