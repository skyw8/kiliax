Edit files by applying a patch in a strict, line-based format.

Format:
- Must start with `*** Begin Patch` and end with `*** End Patch`.
- Operations:
  - `*** Add File: path` (all following lines must start with `+`)
  - `*** Delete File: path`
  - `*** Update File: path` (optionally followed by `*** Move to: new_path`)
- Updates contain one or more hunks starting with `@@` (optional header after it).
- Hunk lines must start with:
  - space: context
  - `-`: delete
  - `+`: add

Rules:
- Paths are workspace-relative only (no absolute paths, no `..`).
- Include enough context lines for hunks to match uniquely (default ~3 lines before/after).

