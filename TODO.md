
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