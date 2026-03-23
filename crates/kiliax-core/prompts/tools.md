## Rules

- Tool arguments MUST be valid JSON.
- For file paths, always use paths relative to the workspace root. Do not use `..`.
- For `read_file`, prefer workspace-relative paths. For skill files, use the absolute paths under the skill directory (start from the `SKILL.md` path listed in the skills section).
- For `shell_command`, always pass `argv` as an array of strings (no shell quoting). If you get a `session_id`, use `write_stdin` to interact/poll.
- For `apply_patch`, pass the patch as a single string in the `*** Begin Patch` / `*** End Patch` format.
- If a tool fails, inspect the error and adjust.
