# Skills

Kiliax discovers skills from:

- `<workspace>/skills/*/SKILL.md`
- `<workspace>/.kiliax/skills/*/SKILL.md`
- `~/.kiliax/skills/*/SKILL.md`

Each skill needs YAML front matter plus concise Markdown. Descriptions matter because agents see metadata before opening the body.

```markdown
---
name: example-skill
description: Trigger-oriented sentence describing exactly when an agent should use this skill.
---

# Example Skill

Short instructions.
```

## Inspect Skills

```bash
curl -sS -H "$AUTH" "$BASE/skills"
```

Use `GET /sessions/<session_id>/skills` for session-scoped discovery.

## Per-Run Overrides

Enable only one skill for a run:

```json
{
  "input": { "type": "text", "text": "Use the kiliax-cli skill." },
  "overrides": {
    "skills": {
      "default_enable": false,
      "overrides": [{ "id": "kiliax-cli", "enable": true }]
    }
  }
}
```

Use session or config settings only when the change should persist beyond one run.
