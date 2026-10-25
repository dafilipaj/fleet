# fleet web UI (React)

The mobile-first PWA (with a keyboard-first desktop layout from 1024px up) for the fleet web server, built with Vite + React 19 + TypeScript +
Tailwind CSS v4 + [shadcn/ui](https://ui.shadcn.com) (radix, "nova" style) + lucide-react.
It talks only to the server's HTTP API ([../ARCHITECTURE.md](../ARCHITECTURE.md) → "HTTP API").

The server serves `dist/` (until it is built, `/` is a "not built" placeholder). `dist/` is gitignored and is
what `fleet install --host` copies — sources and `node_modules` never leave this machine.

## Build

```sh
fleet web build                                  # npm ci (if needed) + npm run build
npm --prefix web/ui ci && npm --prefix web/ui run build   # the same, by hand
```

## Develop

```sh
fleet web serve --port 7796 --bind 127.0.0.1     # an API to talk to (any running server works)
FLEET_WEB_URL=http://127.0.0.1:7796 npm run dev  # Vite on :5173, /api proxied (default target :7777)
```

Checks (all must be clean): `npm run typecheck`, `npm run lint` (oxlint), `npm test` (vitest),
`npm run build`.

## Structure

```
src/
  api/types.ts        API types (Session, Host, FleetResponse, Message, …) — mirror ARCHITECTURE.md
  api/client.ts       typed fetch client: api.fleet(), api.messages(), api.send(), … + ApiError, sessionErrorMessage
  api/spawnWatch.ts   after a spawn: poll the fleet until the new session registers
  lib/format.ts       pure display helpers: relTime, clockTime, shortCwd, sessionSubtitle (first prompt), hostColorSlot
  lib/title.ts        the one session title: sessionTitle() (display_title + optimistic override), validateTitle(),
                      titleChanged(), renameFailure() / tmuxNote() toast copy, echoesTitle()
  lib/sessions.ts     status meta/labels, filters, search, sort, listView(), findSession(), sessionHref(),
                      spawnTargets(), findSpawned(), withoutSession(), clusterByStack() (a stack's rows adjacent)
  lib/chat.ts         detail-view constants + pure helpers (sizes, limits, grouping, interim notes)
  lib/markdown.ts     safe markdown → AST, linkify()
  lib/autoname.ts     naming-pass summaries for toasts / the menu
  lib/models.ts       New session model picker: DEFAULT_MODELS (mirrors lib/config.mjs), normalizeModels(), pickModel()
  lib/groups.ts       Board view: boardColumns() (sessions × /api/groups → columns, Ungrouped last),
                      fallbackGroups()/repoOf() (client-side group-by-repo, worktree-aware),
                      statusSummary(), boardOrder() (↑/↓ order), groupsStatusText(), regroupToast(),
                      applyGroupEdit() (optimistic rename / move), reorderColumns(),
                      withStackColumns() (session stacks → `stack:<id>` columns first)
  lib/stacks.ts       session stacks: memberLive()/memberCounts()/memberCountsText(), sortedMembers(),
                      stacksMissing() (server predates stacks), stacksKnown(), conflictUpdated() (409 body)
  lib/embed.ts        the embed view: parseEmbedHash()/readEmbed() (hash or page query), resolveEmbedDir()
                      (`~` via the host's home, must be a spawn dir or beneath one), isInside(), sessionsIn()
  lib/styles.ts       static Tailwind class maps: status dot/text colours, host badge colours
  lib/shortcuts.ts    desktop keyboard map: BINDINGS (⌘/Ctrl combos), matchShortcut() (key + platform /
                      typing context → action), shortcutHint() (per browser / Fleet.app), isTypingTarget(),
                      stepCursor(), sessionKey(), SHORTCUT_HELP (the ⌘? dialog)
  lib/palette.ts      paletteFilter(): the ⌘K palette's substring matcher / ranking
  lib/scratchpad.ts   the ⌘⇧S scratchpad: autosaved text (localStorage), floating-panel rect clampRect()/defaultRect(),
                      the open-state store scratchpad.show()/hide()/toggle() (components/Scratchpad.tsx renders it)
  lib/paths.ts        chat file paths: isPathLike(), pathTokens(), splitPaths(), pathCandidates() (per message, cached),
                      parsePathRef() (`:line[:col]` / `#L12`), POSIX helpers normalizePath() / dirname() / resolveFrom()
  lib/attach.ts       attachments: insertPaths() (paths at the caret, padded), formatPath() (quote on spaces),
                      uploadName() (clipboard images → pasted-<stamp>.<ext>), overLimit(), formatBytes(), dragHasFiles()
  lib/layout.ts       sidebar width bounds + clampSidebarWidth(); Board flyout bounds: clampFlyoutWidth(),
                      defaultFlyoutWidth(), flyoutMaxWidth(), detailsFitBeside()
  lib/gestures.ts     touch gestures: swipeIntent() (composer chips), pullIntent() (list filter drawer),
                      swipeAxis() / swipeBackIntent() / swipeBackBlocked() (session swipe-back)
  lib/prefs.ts        Settings screen prefs: TEXT_SIZES (Small / Default / Large), rootFontSize() (the <html>
                      font-size that scales every rem), CHAT_FONT_REM, storage keys
  lib/jobs.ts         Bots screen (`#/bots`, old `#/jobs`): flattenJobs() (every host's jobs), runTree() (a run's sessions under their
                      `parent`), liveSession() (host + tmux session → the /api/fleet row), whenLabel(), outcomeTone(),
                      liveSummary(), keepLabel(), promptLabel(), isJobsLocation()
  lib/repos.ts        Settings → Git repos: per-repo interval in a draft (rowInterval()/setRowInterval(), repoKey():
                      the name, or the ~/path for duplicate names), sameSettings(), repoPosition(), syncSummary()
  lib/spawnDirs.ts    Settings → Start directories: the editable draft (DraftRow), fromDraft()/toDraft(), sameList(),
                      validateDraft() (mirrors lib/spawn-dirs.mjs), moveRow(), hostPathErrors()/checksByPath() (a
                      host's answer → its column), baseHost(), divergedHosts(), withOffered()
  lib/palettes.ts     colour palettes (Settings → Theme: Default / Earth / Dusk / Suite, `data-palette` on <html>, tokens
                      in index.css; Suite's in the generated `suite/palette-suite.css`): PALETTES (picker swatches),
                      parsePalette(), themeColor() (meta theme-color per mode)
  suite/              generated, do not edit: `palette-suite.css`, the opt-in **Suite** palette (warm neutrals and a
                      terracotta accent shared with a few sibling apps). It and the app icons in `public/` (and the
                      desktop icons) come from a shared design system kept outside this repo; Default is unchanged
  lib/notes.ts        notes explorer: buildTree(), notesHref() / parseNotesLocation() (`#/notes/<host>/<path>`),
                      indexNotes() + resolveNoteLink() (relative + `[[wiki]]` links), splitRanges(), highlightTerms()
  lib/review.ts       diff reviewer: reviewHref() / parseReviewLocation() (`#/review/<host>/<id>`), fileTree() +
                      treeOrder(), splitRows() (side-by-side pairing), fileSignature() (Viewed resets on change),
                      selectLine() / selectionCode(), placeThreads() (anchors, outdated), orphanThreads()
  lib/outbox.ts       composer outbox (send with an undo delay): pure transitions (schedule / cancel / flush / retry),
                      reconcile() (dedupe optimistic bubbles against the transcript), the Outbox class (timer +
                      one-at-a-time sender), SEND_DELAYS (Settings → Send delay, `fleet.sendDelay`)
  lib/drafts.ts       composer drafts per session: unsent text saved in `fleet.drafts` (saveDraft, 14 days,
                      cleared on send) + one-shot parked text (setDraft) — "Send to session" from a note
  lib/brief.ts        brief helpers: briefTodos(), setTodoItem() (`## Todos`, legacy `## Plan`), groupResources()
                      (RESOURCE_ORDER), gitLine(), editorLabel(), todoProgress(), continueDraft()
  lib/storage.ts      localStorage that never throws
  lib/dormant.ts      dormant sessions (a reboot left them): dormantIndex() / dormantMember() (Board cards, the server's
                      `dormant` flag or the dormant list), dormantTitles() / dormantMeta(), restoreSummary(), dormantMissing();
                      recently closed: hostDormant() / hasEntries() / closedCount() / closedMeta()
  lib/viewport.ts     --app-h / --app-top from visualViewport (utilities h-app / min-h-app / fixed-app)
  lib/utils.ts        cn() (shadcn)
  hooks/usePoller.ts  setTimeout-chained poller: no overlap, pauses while hidden, abort on unmount,
                      refresh() (runs even while hidden)
  hooks/useFollowScroll.ts  follow-the-tail scrolling for the chat
  hooks/useFleet.ts   fleet context + localStorage snapshot (`fleet.snapshot`)
  hooks/useSessionList.ts  list state shared by the mobile list and the desktop sidebar (search, filters, counts)
  hooks/useGroups.ts  useViewMode() (`fleet.view`: list | board), useGroups(enabled): polls /api/groups
                      every 30s (4s while a pass runs) only while the Board is shown; run() = Regroup now;
                      edit() = rename / move (optimistic, POST /api/groups/edit); useBoardColumns() + moveColumn()
  hooks/useStack.ts   useStack(host, id, enabled, ms): one stack (15s in the sheet, 60s for the session's stack bar),
                      `missing` when the server predates stacks
  hooks/useStackUi.ts module store for the app-wide Stack sheet / Spawn sibling form: openStackSheet(),
                      openSiblingSpawn(SiblingTarget), close…()
  hooks/useMediaQuery.ts   useMediaQuery(), useIsDesktop() (≥ 1024px), WIDE_QUERY (≥ 1440px)
  hooks/useTitles.ts  inline-rename store: startEditing/openTitleEditor/stopEditing, useEditing(scope, key),
                      useSessionTitle(s) (optimistic title + saving), useRename() (POST rename, rollback + toast)
  hooks/useLongPress.ts  touch long-press on a row link (swallows the click that follows)
  hooks/useSwipeBack.ts  mobile session screen: swipe right → back (follows the finger, snaps back)
  hooks/useFileLinks.ts  FileLinksContext (what <Markdown> links + what a click does), useFileStats(host, id, onOpen):
                      per-session stat cache, debounced batched POST …/files/stat
  hooks/useAttach.ts  useAttach() (sequential uploads → paths into a textarea, paste handler, progress),
                      useFileDrop() (drop zone + overlay state), usePreventFileNavigation() (app-wide)
  hooks/useNotes.ts   useNotesHosts() (hosts whose fleet entry has `notes`), useNotesTree() (cached, 30s poll),
                      useNotesSearch() (debounced, aborts the previous), useNoteFile()
  hooks/useOutbox.ts  the session screen's Outbox: keepalive POSTs, flush on unmount / background, pagehide hand-off
  hooks/useSettings.ts, useTheme.ts, usePrefs.ts, useNow.ts, usePersistentState.ts
  hooks/useJobs.ts    useJobs() (every reachable host's GET …/jobs, 30s poll, patch() for the enabled switch),
                      useJobShow(host, name, enabled) (one job's runs, 15s poll while expanded)
  hooks/useDormant.ts DormantContext / useDormant(): hosts with dormant / recently closed sessions, index, resume / resumeAll / forget, resumeClosed / forgetClosed
  providers/          DormantProvider (polls every reachable host's /dormant every 30s; restore / forget + toasts),
                      FleetProvider (polls /api/fleet every 5s), SettingsProvider (/api/settings once), ThemeProvider,
                      PrefsProvider (text size → <html> font-size, terminal text, progress notes)
  components/ui/      shadcn components — generated, edit sparingly; add with `npx shadcn@latest add <name>`
  components/         app components: StatusDot, HostBadge/HostDot, SessionRow, EditableTitle, SessionListSkeleton, ScreenHeader,
                      Markdown/Linkified, NewSessionDrawer, ViewToggle (List | Board), DropOverlay,
                      DormantSection (under the list / sidebar, last board column: Resume, Forget, Resume all; ArmButton),
                      ClosedSection (under DormantSection in the list / sidebar, collapsed: recently closed, Resume / Forget),
                      JobChip (a job session's badge on rows / cards: job name, run id in a tooltip), ScheduledToggle
  components/board/   Board (desktop Kanban + header), BoardColumns (columns, drag'n'drop, rename), BoardCard,
                      DormantCard (a dormant group member: dimmed, Resume),
                      GroupedList (mobile collapsible sections),
                      GroupsStatus (last run + Regroup), StatusSummaryDots
  components/stack/   StackUi (mounted once in App: the sheet + the sibling form), StackSheet, StackChip (list rows),
                      StackColumnMenu (board column / mobile section ⋯), StackBar (under the session header)
  components/session/ detail screen parts: ChatView, Composer, OutboxBubbles (pending / sending / sent /
                      failed user bubbles), FilePreview, DetailsPanel (the desktop
                      details column) / DetailsDrawer (mobile ⋯), BriefSection (its top), LatestButton
  components/desktop/ Sidebar (+ SidebarRail when collapsed, NotesLink), CommandPalette (⌘K), ShortcutsDialog (⌘?)
  components/review/  FileDiff (one file's card: unified / split hunks, Viewed, line-number selection, threads under
                      their lines), FileTree, ReviewThread (CommentComposer, ReviewThreadCard)
  components/notes/   NoteTree, NoteResults / RecentNotes, NoteView (frontmatter, body, actions), SendNoteDialog
  screens/            ListScreen (`#/`), SessionScreen (`#/s/:host/:id`; `layout="pane"` on desktop),
                      SettingsScreen (`#/settings`; `layout="pane"` on desktop),
                      NotesScreen (`#/notes[/<host>[/<path>]]`; `layout="pane"` on desktop),
                      JobsScreen (`#/bots`; `layout="pane"` on desktop),
                      ReviewScreen (`#/review/:host/:id`; desktop: file tree | diff | the session),
                      DesktopShell (≥ lg master–detail + the keyboard handler)
  App.tsx             providers + wouter hash router; picks DesktopShell or the mobile screens
```

## Conventions

- **Routing**: wouter with hash location, (`#/`, `#/s/<host>/<id>`; plus `#/settings`, `#/notes…`),
  so no server SPA fallback is needed and old bookmarks/PWA installs keep working.
- **Data**: one `FleetProvider` polls `/api/fleet` for the whole app; screens read it with
  `useFleet()` (`fleet`, `fleetAt`, `error`, `refreshing`, `refresh()`, `applyFleet()`).
  Per-screen loops (messages) use `usePoller(fn, ms)` — `fn` gets an `AbortSignal`; pass it to
  the `api.*` call. Never `setInterval` + fetch.
- **Colours**: tokens in `src/index.css` — shadcn's (`background`, `card`, `muted-foreground`, …)
  plus `dimmer`, `status-{waiting,busy,idle,unknown,error}` and `host-{0..3}`. Use them as Tailwind
  classes (`text-status-busy`, `bg-host-2/10`); keep class names literal (maps in `lib/styles.ts`).
  Dark is the default; light only when the OS prefers it or `fleet.theme` = `light`.
- **Text size**: the Settings text size scales the root font-size, so size things in rem (Tailwind's
  scale, or `text-[0.8125rem]`, not `text-[13px]`); px only for hairlines and fixed chrome. Check
  390px at both Small and Large.
- **Mobile**: ≥ 44px tap targets, inputs ≥ 16px (no iOS zoom; index.css keeps `text-base` fields at
  16px on touch screens at the Small size), `pt-safe`/`pb-safe`/`px-safe` on an
  outer wrapper (they set padding, so put spacing on an inner element), `min-h-app`/`h-app` for
  full-height screens.
- **Desktop vs mobile**: `App` renders `DesktopShell` at ≥ 1024px (`useIsDesktop()`), else the
  mobile `Switch` — two trees, not responsive classes, so the mobile layout cannot drift. Shared
  components take opt-in props (`layout="pane"`, `wide`, `desktop`, `selected`/`cursor`) whose
  defaults keep the mobile markup byte-identical; keep it that way (check a 390px iframe).
- **Storage keys** are `fleet.*`; keep the existing ones stable, viewers' prefs live in them
  (`fleet.chatFont`, `fleet.chatHideNotes`).
- **Text from sessions is data**: render it as text (React escapes); never `dangerouslySetInnerHTML`.
  Markdown must stay DOM-only with `http(s)` links only.
- Pure logic goes in `lib/` with a `*.test.ts` next to it.
- **Titles**: draw a session's name only through `sessionTitle()` / `useSessionTitle()` /
  `<EditableTitle>` — never `s.name` or `s.gen_title` directly. The CLI's `display_title` is the
  one title (docs/architecture.md → Session titles); the tmux name is details-panel metadata only.

## Features

- **Embed view** (`/?host=…&dir=…[&theme=…]#/embed`, also `#/embed?…`): one directory's sessions
  for another app to frame — its own tree in `App.tsx` (`EmbedApp`, `screens/EmbedScreen.tsx`),
  without Fleet's navigation. List (sessions on that host with their cwd in `dir`) + New session
  (the form with host / directory fixed, `SpawnPrefill.fixed`; the session is labelled with `app`, `SpawnPrefill.embed`) → the mobile session screen with
  `embedded` (no Review / scratchpad / stack bar) and `onBack` to the list. `theme` forces the
  mode (`ThemeProvider forced`, and index.html's pre-paint) without saving it. Moves replace
  history entries. Contract: [docs/architecture.md → Embed view](../../docs/architecture.md#embed-view).
- **List** (`#/`): all sessions across hosts, status + host filter chips, search, unreachable-host
  banners, `+` → New session. Mobile: the filter chips (and the Board's regroup line) sit in a
  drawer under the search field, closed by default — swipe down on the header (not in the
  field) or tap the handle to open, swipe up / tap to close; the list itself scrolls as usual.
  While closed, active filters show on the handle ("Needs you · workstation") with ✕ to clear.
  The List | Board toggle stays next to the search field.
- **Bot sessions**: a session a bot started (`origin` on its row) wears a bot chip
  (bot icon + bot name; the run id in a tooltip) on list rows and board cards. **Apps** does the
  same for sessions an app's embedded panel started (`embed`: app icon + app name, the Apps
  filter beside Bots). **Bots** is a status
  filter (the bot-icon chip after Idle, shown when there are job sessions): it lists only job
  sessions. Every other filter leaves them out, except ones waiting on you (they always show, and
  every "needs you" counter counts them); the header counters count every session.
- **Bots** (`#/bots`, `#/jobs` still works; the ⋯ More menu in the mobile list header, the bot icon on desktop, ⌘K → Bots…): every
  host's bots — name, host, schedule, next run, an enabled switch (`POST …/jobs/:name/enabled`,
  optimistic), Run now (a confirm dialog; a 409 because a job session still works offers Run
  anyway = `force`) and the last run's outcome + live sessions. Expanding a job shows its settings
  and its runs (newest first: outcome, when, trigger, reason) with each run's sessions as a tree
  under their `parent`; a session still in /api/fleet (same host + tmux session) shows its live
  status and opens like a list row, else closed / gone. Hosts whose server or CLI predates jobs
  show a one-line note.
- **Inline edits** (`InlineEdit`, the one inline editor — session titles and group names): click
  the text to edit it in place; Enter or clicking away saves, Esc cancels; no edit buttons. The
  field is laid over the text in exactly its box, so nothing moves when it opens.
- **Inline rename** (`EditableTitle`): rows, board cards and the session header show the one
  title; the tmux session name is not on rows any more (it follows the title — details panel only).
  Open the editor with a click on the title (a tap on a touch screen only in the header — on a
  row / card the tap opens it), ⌘E / F2 (the open session's header, else the cursor row/card),
  ⌘K → Rename session…, or a long press on a row (touch). Saving is
  optimistic (spinner) → `POST …/rename`; a 409 (session waiting on a prompt) or an error rolls
  back with a toast; success toasts the new title and the tmux rename.
- **Dormant** (session recovery): sessions a reboot left behind, per host, under the list (mobile
  list, desktop sidebar) and as the last Board column — tmux name, Claude titles, windows/panes,
  how long ago. Resume (one click: it is what you came for), Forget and Resume all (two clicks,
  the first arms for 5 s, like Close session). A dormant group member shows on the Board as a
  dimmed card with Resume. Hidden for hosts whose server / CLI predates it.
- **Board** (`List | Board` toggle next to the search field; `fleet.view`): sessions grouped by
  what they work on. Groups come from `GET /api/groups` (the server's periodic `fleet group`
  pass — stable ids, a 2–4 word label, an optional description); sessions no group claims yet
  go in **Ungrouped** (last), members that are no longer live are dropped, empty groups vanish.
  When grouping is off (`enabled: false`), or the endpoint is missing / failing, the UI groups
  by repo itself (cwd basename; `<repo>/.worktrees/<x>`, `<repo>/worktrees/<x>` and
  `<repo>/.claude/worktrees/<x>` count as `<repo>`) and says "fallback: by repo". Column order is
  sticky (`fleet.boardOrder`): a column keeps its place as statuses change; a new one is
  appended (ordered by needs-you, busy, size, label) and one that comes back returns to its
  slot. Cards within a column put waiting sessions first, then most recent. Opening a card
  never scrolls the board (the flyout may cover it); the ↑/↓ cursor is scrolled clear of it. The list's search / status / host
  filters apply. "Regroup now" (`POST /api/groups/run`, toast with the result) and the last run
  ("grouped 3m ago · 1 model call") sit in the header. Mobile: a grouped list with collapsible
  sections (label, description, status dots, count; collapsed ids in `fleet.groupsCollapsed`)
  of the usual `SessionRow`s. The List view is unchanged.
  Desktop editing (server groups only — not the repo fallback, not Ungrouped or stack columns):
  click a column's name to rename it (Enter / clicking away saves, Esc cancels); drag a card onto
  another column to move the session there, or onto "Drop here for a new group" (shown while
  dragging) and name it; "+ New group" at the end of the board makes an empty column (a group of
  yours: it stays, empty or not, until its "Delete group"); drag a column's header to reorder
  the columns (`fleet.boardOrder`).
  Rename and move are optimistic (`POST /api/groups/edit`) and stick: the grouping pass never
  undoes them.
- **New session** (drawer): host, directory (radio from that host's `spawnDirs`), model (chips
  from `/api/settings` → `models`, Default = no `--model`) and an optional first prompt →
  `api.spawn`. No name field: the server names it (a targeted auto-name pass right after the
  first reply when `web.autoName` is on, else tmux `fw-hhmmss`). 400/409 are shown inline; on success a loading toast watches
  `/api/fleet` (`api/spawnWatch.ts`, 1.5s for up to 45s) for `tmux_session === tmuxSession` and
  opens the session. ⌘/Ctrl+Enter starts it from any field (the prompt included — plain Enter
  there is a newline; the dialog's button shows `⌘↵`). Remembers `fleet.spawnHost` / `fleet.spawnDirLabel.<host>`
  and `fleet.spawnModel` (a model id no longer offered falls back to the first option). The unsent
  first prompt is kept as a draft (`fleet.spawnDraft`, a week) across closing the form and reloads,
  and cleared once the session starts; a prefilled "Continue in new session" never touches it.
- **Session stacks** (docs/architecture.md / ../ARCHITECTURE.md → stacks): N sibling sessions sharing one
  context file, the **StackBrief**. Everything keys off `stack: { id, label, host } | null` on fleet rows —
  `host` is the stack's *home* (where its file is; `stackHome()`), which may differ from the session's host:
  every stack request (sheet, rename, spawn) goes to the home, and rows of one stack cluster / column by
  it whichever hosts they run on. A row without the field (older CLI) and a 404 "not found" / 501 on the
  stack routes (older server) hide the stack UI — no error on list load.
  - **List**: a session in a stack shows a chip (layers icon + label, truncated) in its row's meta
    line; a click opens the Stack sheet (never the row). Rows of one stack stay adjacent: after the
    activity sort each stack moves up to its first member (`clusterByStack`, stable).
  - **Board**: every stack with a shown member is its own column (`stack:<id>`, `source: 'stack'`),
    its members taken out of the smart / repo groups (empty groups vanish; `withStackColumns`, before
    the sticky order — a new stack column starts first, then keeps its slot like any column). Header:
    layers icon, label, status dots, count, ⋯ → StackBrief / Spawn sibling. Stack columns take no
    drops; click the label to rename the stack (like a group: Enter or clicking away saves, Esc
    cancels). Mobile grouped list: the same sections, ⋯ beside the section header.
  - **Rename** (`POST …/stacks/:id/rename`, `useRenameStack` in hooks/useStackLabels.ts): the label is
    editable in place on the board column, the session screen's bar (a click; a tap there opens the
    sheet) and the Stack sheet's title (not while its markdown editor is open). Optimistic: the new
    label shows on every chip / column / bar / sheet at once and wins over `session.stack.label` until
    the fleet has it (≤ 30s); a failure rolls back with a toast.
  - **Session screen**: a slim bar under the header while the session is in a stack — label,
    "2 live · 1 closed" (loaded once, then every 60s), StackBrief, Spawn sibling. Not in one: Details →
    Session → **Spawn sibling…** (the form says it starts a new stack around this session, one Sonnet call).
  - **Stack sheet** (desktop: a large dialog; mobile: full screen): label, home host · cwd · counts ·
    "updated 3m ago · edited"; **Sessions** (live first with their status, closed ones with "closed 2h
    ago", a host dot + name on a member running on another host than the home; a click opens that
    session wherever it runs); **StackBrief** — Summary, Resources (URLs open a new tab),
    Notes, the file path. Edit (pencil) = the whole file incl. frontmatter in a textarea (`label:`
    renames; `## Sessions` is regenerated by fleet); Save (⌘/Ctrl+Enter; Esc cancels) PUTs it with
    `expectUpdated` = the `updated` the edit started from. A 409 ("changed elsewhere") toasts, reloads
    and keeps the edit open with a note — Save again overwrites. Footer: Spawn sibling, Open in
    VS Code / Cursor (`editorUrl`, hidden when absent and on touch screens), Delete stack (click twice
    within 5s; the sessions keep running). Polls the stack every 15s while open.
  - **Spawn sibling**: the New session form in a `sibling` mode — the source (session / stack, its host
    and directory, read-only), a Host choice (default: the source's host; the choice is not remembered),
    the directory fixed to the source's on that host and picked from the host's Start directories on
    any other (the one with the source's folder name preselected), model picker, first prompt (never
    the saved draft). Another host goes in the body as `host` + `dir`; the request still goes to the
    stack's home. A member running on another host than the home spawns from the stack (`stackId`),
    not from itself.
    From a session → `POST …/sessions/:id/stack/spawn` (ensures the stack; "Creating stack…" while the
    model call runs); from a stack → `POST …/stacks/:id/spawn`. A toast says when a stack was created
    (and whether the model wrote the StackBrief); then the usual spawn watch opens the new session.
- **Session detail** (`#/s/:host/:id`), fixed full-screen layout that follows the visual viewport
  (`fixed-app`: `--app-h` + `--app-top`, so the composer stays above the iOS keyboard):
  - header: back, title (click to rename), status, host, "updated Xs ago", ⋯;
  - **swipe right** anywhere = back (also in the installed PWA, which has no native back swipe):
    the screen follows the finger and snaps back unless the swipe is ≥ 70px, mostly horizontal
    and long (≥ 120px) or quick. Left alone: the textarea / inputs, open sheets, a text
    selection, anything that can still scroll left (wide terminal lines, chip rows, code blocks),
    and — in a browser tab — the 20px left edge (the browser's own back swipe);
  - **Chat** polls `messages` every 3s (60 → 200 → 500 with "Load older", which keeps the same
    message under the thumb); bubbles for user / Claude, quiet progress notes (hideable), centred
    command / system lines, time captions per burst, "Claude is working…" / "Needs you" footer;
    markdown via `lib/markdown.ts` (AST, pure) + `components/Markdown.tsx` (React elements only,
    `http(s)` links only);
  - it follows the tail (`hooks/useFollowScroll.ts`): scrolling up pauses and shows "Latest";
    re-renders happen only when the payload changed;
  - errors (`sessionErrorMessage`) as a pill over the pane; host-unreachable / not-in-fleet /
    gone banners; the composer locks for a gone session or `backend: unknown`.
- **Composer**: auto-growing textarea (unsent text is kept per session as a draft across switching and restarts; Enter sends on hardware keyboards, newline on touch;
  1..8000 chars), quick-reply chips from `/api/settings` + built-in keys Esc / Enter / Up / Down (mobile: a mini drawer above the input, hidden until you swipe up on the composer or tap its handle; swipe down hides it; desktop: always shown);
  two follow-up polls after steering. Keys go out at once (a toast per result).
- **Send with undo** (`lib/outbox.ts`, `hooks/useOutbox.ts`): a message (typed or a quick reply) never
  blocks the composer — it clears at once and the message shows as a dashed **pending** bubble with a
  countdown ring (text only with reduced motion), "Sending in 3s", "⌫ to cancel" (desktop) and
  **Undo**; screen readers hear "Sending in 3 seconds, press Backspace to cancel" once. Settings → Chat →
  Send delay: Off / 3 s (default) / 5 s (`fleet.sendDelay`); Off still shows the optimistic bubble.
  - One message counts down at a time: sending another flushes the first immediately and starts
    its own window, so Backspace / Undo always cancels the most recent one. POSTs go one at a time, in
    order, as keepalive fetches.
  - Backspace / Undo puts the text back in the composer — before anything typed since (blank
    line between), caret at the end, focused. While a message is pending, a bare Backspace
    cancels it unless it edits text: outside a text field or in an empty one (the composer right
    after a send); in a non-empty field it deletes as usual. A capture-phase listener; not while
    a dialog or drawer is open. Esc keeps its usual meaning (blur, close the pane / flyout).
  - After the delay: "Sending…", then "Sent" until the next poll brings the real message; the
    optimistic bubble is then dropped. Matching: in order, the first user message after the
    transcript's last user message at send time (or, when that has scrolled out / the chat was
    not loaded, with a timestamp at most a minute before the send) whose text equals it after
    whitespace normalisation (or shares the first 32 chars — Claude Code may rewrite attached
    paths); an unmatched "Sent" bubble expires after 3 min.
  - Failure (409 with the server's reason, 404 gone, host / network unreachable): the bubble shows
    "Not sent: …" with **Retry** (back of the line) and **Edit** (text back in the composer). After
    leaving the session a toast says not sent.
  - Leaving the session (back, another session, the Board flyout switching or closing) sends
    what is still counting down right away; so does the page going to the background (iOS
    freezes timers). On `pagehide` (tab / app closing) the rest go as keepalive fetches, best
    effort.
- **Attachments**: files dropped on the session (anywhere on the desktop pane or the mobile
  screen), pasted into the composer (⌘/Ctrl+V with files on the clipboard — plain text pastes as
  usual) or picked with the paperclip (`multiple`) are uploaded one by one to the session's host
  (`POST /api/hosts/:host/uploads`, see ../ARCHITECTURE.md) and their absolute paths are typed
  at the caret, space-separated, double-quoted only when they contain spaces. A plain absolute
  path (not `@path`, which Claude Code resolves against the cwd) is what Claude reads — images
  included. A dashed "Drop to attach" overlay shows while dragging files; a spinner replaces the
  paperclip and "Uploading <name> (i/n)…" shows under the input; errors (413 too large, host
  unreachable, a host whose server predates uploads) toast. Files over this server's
  `uploads.maxMB` (`/api/settings`) are refused before uploading. The New session form does the
  same for the first prompt (drop on the form, paste, "Attach files"), uploading to the chosen
  host. A file dropped outside a zone never navigates the app away.
- **Voice input**: when `/api/settings` says `transcribe.enabled`, a mic button sits next to Send
  (desktop) or at the start of the chips drawer, beside the paperclip (mobile — both moved there
  so the input keeps its width). Tap to record (the button turns red, "Recording m:ss" with
  Done / Cancel shows under the input, so the drawer can be closed), tap again or Done to stop: the audio goes to `POST /api/transcribe` on this server and the text is appended to the
  message (caret at the end) — never sent by itself. Esc or "Cancel" drops a recording or a
  transcription in flight; recordings stop themselves after 10 minutes. The session's name and
  repo go along as a spelling hint. `getUserMedia` exists only in secure contexts, so the mic
  works on `localhost` and HTTPS origins; on plain `http://<tailscale-ip>` it explains that instead.
  The New session form has the same as a "Voice" button beside "Attach files" (the transcript is
  appended to the first prompt; Start waits until it is in).
- **File links + preview**: paths in Claude's messages — code spans, plain text tokens (with a
  `/` or an extension; URLs, flags, Windows paths, versions and fenced code blocks are skipped)
  and relative markdown links `[t](docs/a.md)`, with optional `:line[:col]` / `#L12` — are sent
  to `POST …/files/stat` (debounced, ≤ 200 per call, cached per session, misses re-asked after
  60s; a host whose server lacks the endpoint is left alone for 5 min). Only existing regular
  files become links (dotted underline, `role="button"`, never an href). A click opens
  `FilePreview`: a full-screen sheet on mobile, a large dialog on desktop. Header: name, path
  (cwd-relative), host, size · age; Download (`files/raw?download=1`), "Open on <host>" (opens it
  with that machine's default app, not in the browser), Copy path, Close (Esc). Content by kind:
  markdown rendered with `<Markdown>` (relative links open in the preview with Back; relative
  images load via `files/raw`; Source toggle, the default when the mention has a line), text with
  line numbers (the line highlighted and scrolled to; first 20k lines), image, PDF (iframe),
  else / over 5 MB an info panel with Download and Open. Markdown safety is unchanged: file
  nodes exist only with `parseMarkdown(text, { files: true })` and render as buttons.
- **⋯ → Details** (drawer, the same content as the desktop details column): name, host · cwd,
  context, **Open in VS Code / Cursor** (`editorUrl`, a plain `vscode://` / `cursor://` link; hidden
  on touch screens), the **brief** (below),
  Auto-name → Run now (+ last run / schedule from `/api/health` when the host is this server),
  session details (tmux, backend, pid, id), Close session (two taps within 5s → `api.kill`, back
  to the list, row dropped optimistically).
- **Brief** (top of Details; web/ARCHITECTURE.md → Session briefs): "updated 3m ago · edited",
  Regenerate, Edit (the body markdown in a textarea; ⌘/Ctrl+Enter saves, Esc cancels — in the
  drawer Esc cancels the edit, not the drawer); Summary; **Todos** with clickable checkboxes and
  `3/7` (checked items are muted, not struck through; toggling rewrites that line of `## Todos`,
  or of a legacy `## Plan`, and PUTs the body); Resources grouped Git (one row: branch · worktree
  / repo + path; legacy Branch / Worktree rows too), Pull requests, Issues, Artifacts, Specs,
  Links, Files (collapsed by default with a count; `fleet.briefFilesOpen`), Notes — files open the
  preview, URLs a new tab; **Continue in new session** (the New session form prefilled with the
  host, the session's cwd and `continuePrompt`, caret at the end). GET while Details is open,
  every 30s, every 2.5s while generating.
- **Settings** (`#/settings`: the ⋯ More menu in the list header on mobile, the top bar on desktop, ⌘K → Settings…): Text size (Small / Default /
  Large = chat at 13 / 15 / 17px, `fleet.chatFont`) scales the **whole** UI via the `<html>`
  font-size (index.html applies it before first paint); Theme (`fleet.theme`); Progress notes (`fleet.chatHideNotes`); Send
  delay (`fleet.sendDelay`, above). Per
  viewer (localStorage), for every session. **Git repos** (`components/ReposEditor.tsx`) is per host
  (a host switcher): Auto-sync (the `fleet.repos` launchd timer, on/off), the default interval, the
  scanned directories, and every repo with its branch, `↓↑✎` position, last sync and outcome; each row
  has Sync now and an interval picker (Default / Every 30m…7d / Off = excluded). Save writes the shown
  host; Save to all hosts writes the same settings everywhere; Sync all now runs `fleet repos sync`.
  **Start directories** (`components/SpawnDirsEditor.tsx`)
  is fleet-wide instead: the `spawnDirs` the New session form offers, one row per label with a
  path field per host (hosts from `/api/fleet`; empty = not offered there), add / rename / delete /
  move up-down. Loaded with `GET …/spawn-dirs` from every host (this server's list is the one
  edited; a host with a different one is named). Each cell shows that host's own check (✓ is a
  directory, ✗ missing / not a directory) from a debounced dry-run `PUT` of the draft; labels and
  path syntax are checked as you type. Save (enabled when dirty and valid; Revert resets; the
  browser asks before leaving with unsaved changes) dry-runs on every reachable host, then writes
  the same list to each, lists per-host results (unreachable hosts skipped with a warning) and
  patches the fleet snapshot with what each host now `offered`, so the New session form shows it
  at once. Mobile: one card per entry; from `md` up a compact grid.
- **Diff reviewer** (`#/review/<host>/<id>`; ../ARCHITECTURE.md → `…/diffs`): GitHub's "Files
  changed" over every checkout the session works in — its cwd's and the worktrees it edits from
  there (a meta repo with several PRs in flight), each since the merge-base with its default
  branch, uncommitted and untracked files included. Several checkouts: a section per checkout
  (label, branch ← base, +/−) and the file tree grouped by checkout; clean ones are left out.
  Files are keyed by root + path (`fileKey`), threads carry their `root`. Reached from the diff button in the session header
  and ⌘K → Review changes; Esc goes back to the session. `useDiff` polls every 2 s with
  `since=<fingerprint>` (an unchanged tree answers without files), so the diff follows the
  session as it edits; a file whose diff just changed is ringed for 10 s and highlighted in the
  tree. Unified / split (`fleet.diffView`), collapse all, per-file Viewed (stored per session in
  `fleet.reviewViewed:<host>/<id>` as the diff's signature, so a new change unticks it). Desktop:
  file tree (`fleet.reviewTree`) | diff | the session itself as a pane (`fleet.reviewChat`) —
  review and conversation side by side.
  **Comments** (`useReview`, ../ARCHITECTURE.md → `…/review`): select code with the mouse → a
  "Comment on lines X–Y" button at the selection's end (`rowsToRange` over the rows' `data-*`;
  split view: the side the selection ends on), or click a line number (a "+" shows on the
  hovered row; shift-click extends on the same side) → a composer under the lines: **Add single comment** types it into
  the session now; **Start a review** / **Add review comment** keeps it as a pending draft, and
  **Submit review (n)** in the header sends all drafts in one prompt with an optional overall
  note (⌘↵ in the composer: a review comment once one is pending, else a single comment). Each
  thread sits under its last line with the session's answer (final messages; interim narration
  only until there is one), its state (pending / sent / queued / Claude is working…), Reply,
  Resolve and Remove. A thread whose lines changed is *Outdated*; one whose lines left the diff
  shows at the top of its file, one whose file left the diff at the bottom. Polled every 2 s
  while an answer is due, else every 10 s.
- **Notes** (`#/notes[/<host>[/<path>]]`; ../ARCHITECTURE.md → notes): browse, search and read the
  markdown notes of any host with `web.notes.root` (hosts come from `/api/fleet` entries carrying
  `notes`; the last one used is `fleet.notesHost`). Reached from the ⋯ More menu in the mobile
  list header, the desktop top bar, ⌘K → Notes… (the palette also
  lists the first notes host's 300 most recent notes) and ⌘⇧E. Folder tree (expanded folders
  per host in `fleet.notesOpen.<host>`, the open note's folders expand), search (180 ms debounce,
  previous request aborted; words AND-ed, `"phrases"`, `#tag`; Enter opens the first hit, Esc
  clears) with snippets and the matches marked, "Recently changed" when there is no query. A
  note: title, host, path, age; Source / Rendered, Copy path, **Send to session** (pick a session
  on the note's host: it opens with the absolute path in its composer, nothing is sent), **Open in
  VS Code / Cursor** (`editorUrl`, hidden on touch); frontmatter as a compact key → value grid
  (tags as chips, URLs linked); the body via `<Markdown notes>` — `[[wiki]]` links (path, then
  relative, then by file name, nearest folder first), relative `[t](other.md)` links and images
  resolve against the tree and open inside the explorer (a missing target is dashed text);
  words of the active search are marked and a note opened from a hit scrolls to the first mark.
  Encrypted blocks show a notice. Desktop: the explorer replaces the sidebar / board — header
  (← Sessions, name, host picker, search ⌘F, root, editor link), then tree | results or recent |
  note. Mobile: the browse/search screen (recent + tree, or results), a note is its own screen;
  swipe right = back on both.
- **PWA**: `public/` has the manifest (standalone, `/#/`) and the app icons (generated, like
  `src/suite/`, from the shared design system: a white arrowhead on a terracotta tile; the 512 PNG
  is `any maskable`);
  `index.html` sets theme-color (kept in sync with the theme), apple-mobile-web-app meta,
  `viewport-fit=cover` and `interactive-widget=resizes-content`. No service worker (the app is
  useless offline; the server revalidates every file).

## Desktop (≥ 1024px)

Master–detail on the same hash routes as mobile (`#/` = nothing open, `#/s/<host>/<id>` = that
session in the pane), so a link opens the same session on either layout.

- **Top bar** (the same over List and Board): Fleet summary, List | Board, search (⌘F),
  Sessions | Bots toggle with status and host pickers, modules, `+`.
- **Sidebar** (left, List only): the session list (the open row is highlighted, the keyboard
  cursor has a ring), Dormant and Recently closed. Resizable by dragging its edge (280–560px,
  double-click resets, ←/→ on the focused handle; `fleet.sidebarWidth`).
- **Pane**: the session screen without the back button; header adds the cwd; chat is a wider
  (max-w-4xl) readable column, the terminal uses the full width; the composer sends on Enter (also
  ⌘/Ctrl+Enter, touch-capable laptops included), Shift+Enter is a newline. Nothing open → an empty
  state with key hints and the sessions that need you.
- **Board** (⌘B, the toggle in the sidebar / board header, or ⌘K): the Kanban replaces the
  sidebar — full width, one column per group (label, status dots, count, description), cards
  with status, name, host, subtitle, status / `waiting_for`, context meter and age. Clicking a
  card (or ↑/↓ + Enter, walking the columns left to right) opens the session as a non-modal
  flyout over the board's right edge (`#/s/<host>/<id>`, same route as the list): the board keeps
  its width and layout, stays scrollable (the last columns scroll out from under the flyout) and
  clickable — another card switches the flyout. Its left edge resizes it (drag, or focus it and
  ←/→, Shift for bigger steps; double-click resets to 55% of the board; min 420px, max the board
  minus 240px; `fleet.flyoutWidth`). Esc (outside a field) or ✕ closes it.
- **Details panel** (right of the chat, always open): resizable from its left edge like the
  sidebar (260–560px, default 320px, double-click resets, ←/→ on the focused handle;
  `fleet.detailsWidth`); the chat keeps at least 380px, and a pane too narrow for both shows the
  ⋯ button (the drawer) instead. The mobile ⋯ drawer's content (`DetailsPanel`) — the brief first
  (summary, todos, resources, continue), then auto-name,
  tmux/backend/pid/id, Close session (click twice).
- **New session**: the mobile form in a dialog. Toasts sit bottom-right, above the composer.
- **Polling** is unchanged: one `/api/fleet` loop (FleetProvider, 5s) feeds the sidebar and the
  pane's status; the pane runs one messages (3s) loop. The tab title is
  `(<needs you>) <session> · Fleet`.

### Shortcuts

Every action is ⌘+key on macOS, Ctrl+key elsewhere (`mod`; on macOS Ctrl stays the fields' own
Emacs-style editing keys). No action combo but ⌘⌫ is a text-editing one, so they work in the composer and
the search field too; ⌘A/C/V/X/Z, ⌘ + arrows, ⌥ + arrows, ⌘Enter keep their native meaning there.
Only navigation is unmodified: ↑/↓ and Enter outside a field, Esc. With a dialog open only ⌘K
(and ⌘? to close the help) works. One `keydown` listener in `DesktopShell` maps keys through
`matchShortcut()`; the help dialog, the ⌘K palette hints and the button tooltips come from the same
`BINDINGS` table, so they show this environment's combo.

Browsers keep some combos for themselves (⌘N/T/W/Q, ⌘⇧N/T cannot be caught; ⌘L, ⌘R, ⌘1–9, ⌘[ ⌘]
are the address bar, reload, tabs, history — Fleet.app's menu uses ⌘R, ⌘⇧R, ⌘W, ⌘[, ⌘] the same
way), so the scheme avoids them. New session is the exception: ⌘N in Fleet.app — a native **File →
New Session** menu item that dispatches a `fleet:command` DOM event into the page — and ⌘⇧O (the
"new chat" combo of chat apps) in a browser tab and in the app. Everything is also in ⌘K.

| Keys | Action |
| --- | --- |
| `↓` / `↑` | move the list cursor (only when focus is not in the chat/terminal) |
| ⌥`↓` / ⌥`↑` | move the list cursor from the chat/terminal too (not in a field) |
| `Enter` | open the cursor's session and focus the composer (the open one: focus the composer) |
| ⌘/Ctrl+`F` | search (in the field: ↑/↓ move, Enter opens, Esc clears then leaves; ⌘F again: the browser's find) |
| ⌘`N` (Fleet.app), ⌘/Ctrl+⇧`O` | new session |
| ⌘/Ctrl+`E`, `F2` | rename (the open session, else the cursor row / card) |
| ⌘/Ctrl+`⌫` | close the session (the open one, else the cursor row): press twice within 3 s; outside a field only (there it deletes to the line start) |
| `Enter`, ⌘/Ctrl+`Enter` · `Shift+Enter` | send · newline (composer) |
| `Esc` | cancel a pending send (first, while one counts down); leave the field; otherwise close the pane or Settings (`#/`) |
| ⌘/Ctrl+`Enter` | New session form: start |
| ⌘/Ctrl+`B` | switch List / Board |
| ⌘/Ctrl+`\` | toggle the sidebar |
| ⌘/Ctrl+`I` | toggle the details panel (brief + session details) |
| ⌘/Ctrl+⇧`E` | notes explorer (again, or `Esc`: back to the sessions) |
| ⌘/Ctrl+`K` | command palette: jump to any session (all hosts, ignores filters), actions (Settings…, Refresh now, Open in VS Code), filters, theme |
| ⌘/Ctrl+`?`, ⌘/Ctrl+`/` | shortcuts help |
