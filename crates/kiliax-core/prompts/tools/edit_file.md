Edits text files.
- From `read_file` output, use only the content after `N: ` in `oldString`/`newString`.
- Empty `oldString` creates or replaces a whole file with `newString`.
- Prefer editing existing files; create files only when required.
- If `oldString` is missing or ambiguous, use more context or `replaceAll`.
- Avoid emojis unless explicitly requested.
