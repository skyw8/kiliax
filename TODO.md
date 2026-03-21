
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

TUI细节修改
- 添加计时，工具调用、思考过程都计时显示，最后输出完毕之后显示总计时
- 思考部分使用codex样式的灰色斜体，参考codex /home/skywo/github/codex/codex-rs/tui
- 默认折叠工具调用，显示概要信息即可，参考codex
- 调用工具时旁边显示计时
- 用户输入时开始计时，计时显示在输入框上方，任务完成后，最后使用一个分割线记录总时间（灰色），参考codex
- 编辑少量代码时，像codex一样diff显示。创建文件写入或编辑大量代码时仅显示概要信息和文件路径。
- 省略User Assistant显示，用户输入被之前输入框的灰色背景包裹即可
