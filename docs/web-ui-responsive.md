# Web UI responsiveness & robustness (mobile-first notes)

This document summarizes the current approach used in Kiliax’s React web UI to stay usable and visually consistent on:

- narrow desktop windows
- tablets
- phones (including safe-area / iOS home indicator)

It is written for React beginners, but focuses on practical patterns rather than React theory.

Last updated: 2026-04-12.

## Goals

1. Keep the layout readable at small widths (no “only looks good fullscreen”).
2. Prevent horizontal overflow caused by long strings (model IDs, workspace paths, tool call JSON).
3. Keep interaction patterns consistent across desktop vs. mobile (sidebar, menus).
4. Avoid complexity: prefer simple CSS/Tailwind solutions over heavy components unless necessary.

## Breakpoints & definitions

Tailwind defaults (important for reading the class names):

- `sm`: ≥ 640px
- `md`: ≥ 768px
- `lg`: ≥ 1024px
- `xl`: ≥ 1280px

In the app we treat **mobile / narrow viewport** as `< md` (width ≤ 767px):

- `isOverlaySidebarViewport()` / `useOverlaySidebarViewport()` in `web/src/app.tsx`

## Core layout strategy

### 1) Constrain the main content width

Instead of trying to fill the whole screen, we center the chat and composer in a stable container:

- `max-w-4xl` + `mx-auto` in `web/src/app.tsx`

This keeps line lengths reasonable on ultra-wide screens and makes smaller windows degrade gracefully.

### 2) Use `dvh` and safe-area insets

Mobile browsers have dynamic toolbars; `100vh` is often wrong. We use:

- `h-dvh` for the app root (dynamic viewport height)
- `pb-[env(safe-area-inset-bottom)]` (or `calc(...)`) for bottom spacing near the iOS home indicator

See the app root and the composer container in `web/src/app.tsx`.

### 3) Stop flex children from “refusing to shrink”

Most truncation issues in flex layouts come from missing `min-w-0`.

Rules used across the UI:

- If a flex item contains text that must truncate: add `min-w-0` on the flex child.
- Use `truncate` on the text node (and keep it in a container that can shrink).

Example: session rows, header metadata, workspace display.

## Desktop vs. mobile patterns

### Sidebar: persistent on desktop, drawer on mobile

Desktop can afford a fixed sidebar; mobile cannot.

- Desktop: `<aside className="w-[280px] ...">`
- Mobile (`isNarrowViewport`): render the same sidebar content inside a Radix `Sheet`

Code path: `web/src/app.tsx` + `web/src/components/ui/sheet.tsx`.

Why this works:

- The information architecture stays identical.
- Only the “container” changes (aside vs. drawer).

### Menus: anchored popover on desktop, ActionSheet on mobile

Small screens need bigger tap targets and less precise positioning.

- Desktop: small anchored menus (positioned using the clicked button’s `getBoundingClientRect()`)
- Mobile: bottom ActionSheet with large buttons

Code path:

- Desktop menus: `web/src/app.tsx` (fixed positioned menu blocks)
- Mobile sheets: `web/src/components/ui/action-sheet.tsx` + `web/src/app.tsx` usage

### Composer: keep input centered; tools are optional and independent

The composer is designed so the **input stays centered**, and optional tools do not affect centering.

Current behavior:

- Mobile / narrow: only the input pill (plus Send/Interrupt)
- Wide screens: an additional “Quick open tools” pill exists as a separate element

Implementation notes:

- The tools pill is **absolutely positioned relative to the centered container**, placed to the right (`left-full` + `translate-x-*`).
- It is only shown at `xl` and above to avoid clipping on “not quite wide enough” windows.

See: `web/src/app.tsx` around the bottom composer block.

## Handling long strings (robustness techniques)

### 1) Human-friendly labels with full value available on hover/copy

Long IDs should not destroy layout. The pattern is:

1. Render a short label in the UI.
2. Put the full string in `title=...` for hover tooltip.
3. Provide click-to-copy for power users.

Model IDs:

- `splitModelId()` + `modelLabel()` in `web/src/app.tsx`
- UI shows `model (provider)` instead of `provider/model`
- Full ID is kept in the `title` attribute

Workspace path / temp workspaces:

- `workspaceBasename()` + `workspaceDisplayName()` in `web/src/app.tsx`
- Temporary workspace names like `tmp_2026...` use a **middle ellipsis** (`prefix…suffix`) to stay readable.
- Full path is available via `title` and click-to-copy.

### 2) Force text to wrap instead of overflowing

User messages:

- `whitespace-pre-wrap break-words` on the user bubble in `web/src/app.tsx`

Markdown output:

- `break-words` on the Markdown root in `web/src/components/markdown.tsx`

Tool call JSON / code:

- Code blocks render in a dedicated component (`CodeBlock`) which uses scrolling instead of expanding the layout.

### 3) Badges must never wrap

The sidebar status badge (e.g. “step 1”) must remain a single line.

- `whitespace-nowrap` is built into the badge base styles in `web/src/components/ui/badge.tsx`.

## Dialogs on small screens

Dialogs are implemented using Radix `Dialog`, with a “full-screen-ish” inset layout on small screens:

- On narrow screens: `fixed inset-3` + `max-h` + `overflow-y-auto`
- On `sm` and up: center the dialog with a fixed max width

See: `web/src/components/ui/dialog.tsx`.

## Debugging & verification checklist

Use Chrome/Firefox devtools device toolbar and test these widths:

- 320px (small phone)
- 375–430px (common phones)
- 768px (tablet breakpoint)
- 1024px (small laptop)
- 1280px (desktop where quick tools appear)

Stress tests (use real data):

- A very long model ID (with provider prefix)
- A temporary workspace name containing a long `tmp_...` suffix
- Tool call JSON with deeply nested objects
- Long unbroken strings in messages (URLs, hashes)

What to look for:

- No horizontal scrolling on the page (unless inside code blocks)
- Header controls don’t overlap; selects remain usable
- Sidebar drawer opens/closes smoothly on mobile
- ActionSheet buttons are easy to tap

## When to upgrade components (and when not to)

We currently use native `<select>` for agent/model because it is:

- fast
- accessible by default
- minimal dependency surface

If model lists become large or need search, a good next step is a combobox (e.g. shadcn/Radix + `Command`) with:

- type-to-filter
- grouped providers
- ellipsized selected label + full tooltip

Only do this when the native select stops being usable; otherwise keep it simple.

