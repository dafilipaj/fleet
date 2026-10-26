// Session briefs: the file format (docs/architecture.md → "Session briefs"). Pure functions,
// no I/O — shared by the brief service (lib/briefs.mjs) and anything else that reads or writes
// the files. `fleet brief` (crates/fleet/src/core/brief.rs) is a port: keep the two in step —
// both pin the fixtures in testdata/briefs/ (regenerate with `node testdata/briefs/gen.mjs`).
//
//   ---
//   session: <session id>
//   host: <host name>            cwd: <directory>        (written by the server, informational)
//   updated: <iso>               last write of any kind
//   generatedThrough: <n>        transcript BYTE offset the summary has consumed
//   generatedAt: <iso>           last model generation
//   editedAt: <iso>              last human edit (omitted until there is one)
//   todos: <hash>                the todo state last copied into ## Todos
//   dismissed: ["key", …]        resources the user deleted; never re-added
//   git: <line>                  the auto `Git:` resource line last written (set: legacy auto
//                                `Branch:` / `Worktree:` lines have been migrated)
//   dirs: ["/abs", …]            working directories the session was in (server-only, an
//                                unknown key to the CLI; feeds the Worktrees list)
//   ---
//   ## Summary / ## Resources / ## Todos   (any other `## ` section is kept, after Todos;
//                                           a legacy `## Plan` reads as Todos)
//
// A human edit is authoritative: parsing tolerates missing sections, extra text and sections,
// `*` bullets and `[X]`; serialising writes the three sections in canonical order.

export const SECTIONS = ['Summary', 'Resources', 'Todos'];
export const RESOURCE_KINDS = ['Origin', 'PR', 'Issue', 'Artifact', 'Spec', 'File', 'Git', 'Branch', 'Worktree', 'Link'];
const KIND_BY_LOWER = new Map(RESOURCE_KINDS.map((k) => [k.toLowerCase(), k]));

/** Frontmatter keys in the order they are written; unknown keys follow, as found. */
const META_ORDER = ['session', 'host', 'cwd', 'updated', 'generatedThrough', 'generatedAt', 'editedAt', 'todos', 'dismissed', 'git'];
export const MAX_DISMISSED = 200;

const URL_RE = /https?:\/\/[^\s<>"'`)\]]+/g;
const HEADING_RE = /^##\s+(.+?)\s*#*\s*$/;
const BULLET_RE = /^\s*[-*+]\s+(.*)$/;
const CHECK_RE = /^\s*[-*+]\s+\[([ xX])\]\s+(.*)$/;

function oneLine(v) {
  return String(v).replace(/[\r\n]+/g, ' ').trim();
}

// ------------------------------------------------------------------ frontmatter

/** Split `---\n…\n---` off the top. → { meta, body }. No frontmatter → meta {}. */
export function parseFrontmatter(text) {
  const src = String(text ?? '').replace(/\r\n/g, '\n');
  const m = /^---\n([\s\S]*?)\n---[ \t]*(?:\n|$)/.exec(src);
  if (!m) return { meta: {}, body: src };
  const meta = {};
  for (const line of m[1].split('\n')) {
    const kv = /^([A-Za-z][\w-]*)\s*:\s*(.*)$/.exec(line);
    if (!kv) continue;
    const [, key, raw] = kv;
    let v = raw.trim();
    if (!/^[[{"]/.test(v)) v = v.replace(/\s+#.*$/, ''); // a trailing `# comment`
    if (v === '' || v === 'null' || v === '~') continue;
    if (/^-?\d+$/.test(v)) meta[key] = Number(v);
    else if (/^[[{"]/.test(v)) {
      try {
        meta[key] = JSON.parse(v);
      } catch {
        meta[key] = v;
      }
    } else meta[key] = v;
  }
  return { meta, body: src.slice(m[0].length) };
}

export function serializeFrontmatter(meta = {}) {
  const keys = [...META_ORDER.filter((k) => k in meta), ...Object.keys(meta).filter((k) => !META_ORDER.includes(k))];
  const lines = [];
  for (const k of keys) {
    const v = meta[k];
    if (v == null || v === '' || (Array.isArray(v) && v.length === 0)) continue;
    if (typeof v === 'number') lines.push(`${k}: ${v}`);
    else if (typeof v === 'object') lines.push(`${k}: ${JSON.stringify(v)}`);
    else {
      const s = oneLine(v);
      // Quote what would read back as another type (a number, JSON, a comment).
      lines.push(`${k}: ${/^(-?\d+|null|~)$|^[[{"]|\s#/.test(s) ? JSON.stringify(s) : s}`);
    }
  }
  return `---\n${lines.join('\n')}\n---\n`;
}

// ------------------------------------------------------------------ resources / todo lines

/** Strip markdown noise from a URL's surroundings: trailing punctuation, a closing paren run. */
export function cleanUrl(url) {
  let u = String(url).replace(/[.,;:!?'"]+$/, '');
  while (u.endsWith(')') && (u.match(/\(/g)?.length ?? 0) < (u.match(/\)/g)?.length ?? 0)) u = u.slice(0, -1);
  return u;
}

/** The identity of a resource for de-duplication and `dismissed`: its URL, else its path. */
export function resourceKey(r) {
  if (r.kind === 'Git') return 'git'; // one per brief: replaced in place, dismissed as a whole
  if (r.url) return r.url.replace(/#.*$/, '').replace(/\/+$/, '');
  if (r.path) return `${r.kind === 'Branch' || r.kind === 'Worktree' ? `${r.kind.toLowerCase()}:` : ''}${r.path}`;
  return (r.text ?? '').trim().toLowerCase();
}

/**
 * A `Git:` value — ``\`branch\` · worktree \`~/path\``` (a linked worktree) or ``· repo \`~/path\```
 * (the main checkout); `detached` in place of the branch. → { branch, path, linked }.
 */
function parseGitValue(value) {
  const root = /(?:^|\s)(worktree|repo)\s+`([^`]+)`/i.exec(value);
  const br = /^`([^`]+)`/.exec(value);
  const branch = br && (!root || br.index < root.index) ? br[1].trim() || null : null;
  return {
    branch,
    path: (root ? root[2] : branch ? '' : value).trim() || null,
    linked: root ? root[1].toLowerCase() === 'worktree' : null,
  };
}

/** One Resources bullet (without the `- `) → { kind, label, url, path, text, branch, linked, key }. */
export function parseResourceLine(text) {
  const t = String(text).trim();
  let kind = null;
  let value = t;
  const km = /^([A-Za-z][A-Za-z ]{0,15}?)\s*:\s+(.*)$/.exec(t);
  if (km && KIND_BY_LOWER.has(km[1].toLowerCase())) {
    kind = KIND_BY_LOWER.get(km[1].toLowerCase());
    value = km[2].trim();
  }
  if (kind === 'Git') {
    const g = parseGitValue(value);
    const r = { kind, label: g.branch ?? g.path, url: null, path: g.path, text: t, branch: g.branch, linked: g.linked };
    return { ...r, key: resourceKey(r) };
  }
  let url = null;
  let label = null;
  let p = null;
  const link = /\[([^\]]*)\]\((https?:\/\/[^)\s]+)\)/.exec(value);
  if (link) {
    label = link[1].trim() || null;
    url = link[2];
  } else {
    const u = value.match(URL_RE);
    if (u) url = cleanUrl(u[0]);
  }
  if (!url) {
    const bt = /`([^`]+)`/.exec(value);
    p = (bt ? bt[1] : value).trim() || null;
    label = p;
  }
  if (!kind) kind = url ? classifyUrl(url).kind : null;
  const r = { kind, label: label ?? url, url, path: p, text: t, branch: null, linked: null };
  return { ...r, key: resourceKey(r) };
}

/** → `PR: [owner/repo#12](url)`, `File: \`path\``, `Git: \`b\` · repo \`p\``, … (the bullet text, no `- `). */
export function formatResource(r) {
  const kind = r.kind ?? 'Link';
  const code = (v) => `\`${String(v ?? '').replace(/`/g, '')}\``;
  if (kind === 'Git') return `Git: ${r.branch ? code(r.branch) : 'detached'} · ${r.linked ? 'worktree' : 'repo'} ${code(r.path)}`;
  if (r.url) return `${kind}: [${(r.label ?? r.url).replace(/[[\]]/g, '')}](${r.url})`;
  return `${kind}: \`${String(r.path ?? '').replace(/`/g, '')}\``;
}

/** Classify a (cleaned) URL. → { kind, label, url } (url canonicalised for GitHub PRs/issues). */
export function classifyUrl(url) {
  const gh = /^https?:\/\/github\.com\/([\w.-]+)\/([\w.-]+)\/(pull|issues)\/(\d+)/.exec(url);
  if (gh) {
    const [, owner, repo, what, n] = gh;
    return {
      kind: what === 'pull' ? 'PR' : 'Issue',
      label: `${owner}/${repo}#${n}`,
      url: `https://github.com/${owner}/${repo}/${what}/${n}`,
    };
  }
  const art = /^https?:\/\/claude\.ai\/(?:code\/)?artifacts?\/([A-Za-z0-9-]{6,})/.exec(url);
  if (art) return { kind: 'Artifact', label: `artifact ${art[1].slice(0, 8)}`, url: url.replace(/[?#].*$/, '') };
  let label = url;
  try {
    const u = new URL(url);
    label = `${u.host}${u.pathname === '/' ? '' : u.pathname}`;
    if (label.length > 80) label = `${label.slice(0, 77)}…`;
  } catch {
    /* keep the raw url */
  }
  return { kind: 'Link', label, url };
}

export function parseTodoLine(line) {
  const m = CHECK_RE.exec(line);
  return m ? { done: m[1] !== ' ', text: m[2].trim() } : null;
}

export function formatTodos(items) {
  return items.map((i) => `- [${i.done ? 'x' : ' '}] ${oneLine(i.text)}`).join('\n');
}

// ------------------------------------------------------------------ whole brief

/**
 * Parse a brief (with or without frontmatter). →
 *   { meta, preamble, summary, resourcesText, todosText, extra: [{ heading, body }],
 *     resources: [parsed lines], todos: [{ done, text }] }
 * `## Plan` (the section's name before it became Todos) reads as `## Todos`.
 */
export function parseBrief(text) {
  const { meta, body } = parseFrontmatter(text);
  const known = { summary: [], resources: [], todos: [] };
  known.plan = known.todos;
  const extra = [];
  const pre = [];
  let cur = pre;
  for (const line of body.split('\n')) {
    const h = HEADING_RE.exec(line);
    if (h) {
      const name = h[1].trim().toLowerCase();
      if (name in known) cur = known[name];
      else {
        const sec = { heading: h[1].trim(), lines: [] };
        extra.push(sec);
        cur = sec.lines;
      }
      continue;
    }
    cur.push(line);
  }
  const join = (lines) => lines.join('\n').replace(/^\s*\n/, '').trimEnd();
  const resourcesText = join(known.resources);
  const todosText = join(known.todos);
  const resources = [];
  for (const line of resourcesText.split('\n')) {
    const b = BULLET_RE.exec(line);
    if (b && b[1].trim()) resources.push(parseResourceLine(b[1]));
  }
  const todos = todosText.split('\n').map(parseTodoLine).filter(Boolean);
  return {
    meta,
    preamble: join(pre),
    summary: join(known.summary),
    resourcesText,
    todosText,
    extra: extra.map((s) => ({ heading: s.heading, body: join(s.lines) })),
    resources,
    todos,
  };
}

/** A parsed (possibly modified) brief → canonical text. */
export function serializeBrief(b) {
  const parts = [serializeFrontmatter(b.meta ?? {})];
  if (b.preamble) parts.push(`${b.preamble}\n\n`);
  parts.push(`## Summary\n${b.summary ? `${b.summary}\n` : ''}\n`);
  parts.push(`## Resources\n${b.resourcesText ? `${b.resourcesText}\n` : ''}\n`);
  parts.push(`## Todos\n${b.todosText ? `${b.todosText}\n` : ''}`);
  for (const s of b.extra ?? []) parts.push(`\n## ${s.heading}\n${s.body ? `${s.body}\n` : ''}`);
  return parts.join('');
}

export function emptyBrief(sessionId) {
  return parseBrief(serializeBrief({ meta: { session: sessionId } }));
}

/**
 * Add auto-extracted resources to the Resources section: every existing line is kept as it is
 * (hand-written ones included), new items are appended unless their key is already there or
 * was dismissed. An `Origin` item whose link is already there as an auto `Link:` / `Issue:`
 * line turns that line into its `Origin:` one. → { text, added } (`added` counts those too).
 */
export function mergeResources(resourcesText, items, dismissed = [], { max = 80 } = {}) {
  const existing = String(resourcesText ?? '').trimEnd();
  const lines = existing ? existing.split('\n') : [];
  const have = new Set();
  const upgradable = new Map(); // key → index of an auto Link / Issue line
  let count = 0;
  lines.forEach((line, i) => {
    const b = BULLET_RE.exec(line);
    if (!b) return;
    count += 1;
    const r = parseResourceLine(b[1]);
    have.add(r.key);
    if (r.url) have.add(resourceKey({ url: classifyUrl(r.url).url }));
    if ((r.kind === 'Link' || r.kind === 'Issue') && r.url && formatResource(r) === b[1].trim()) upgradable.set(r.key, i);
  });
  const skip = new Set(dismissed);
  let added = 0;
  for (const item of items) {
    const key = resourceKey(item);
    if (item.kind === 'Origin' && upgradable.has(key)) {
      const i = upgradable.get(key);
      const r = parseResourceLine(BULLET_RE.exec(lines[i])[1]);
      lines[i] = `- ${formatResource({ ...r, kind: 'Origin' })}`;
      upgradable.delete(key);
      added += 1;
      continue;
    }
    if (!key || have.has(key) || skip.has(key)) continue;
    if (count >= max) break;
    lines.push(`- ${formatResource(item)}`);
    have.add(key);
    count += 1;
    added += 1;
  }
  return { text: lines.join('\n'), added };
}

/** What the server wrote before the one `Git:` line: a `- Branch:` / `- Worktree:` code-span line. */
const LEGACY_GIT_RE = /^- (?:Branch|Worktree): `[^`]+`$/;

/**
 * Put the auto `Git:` line (branch + repo / worktree root, `item` = { kind: 'Git', branch, path,
 * linked }) into the Resources section, once:
 *   - a Git line in the canonical form (what formatResource writes) is the auto one: replaced in
 *     place when the branch or root changed;
 *   - no Git line at all → inserted (at the top, or where the legacy lines were), unless `git`
 *     was dismissed (a human deleted it);
 *   - a Git line in any other form is a human's: left alone (and none is added).
 * `migrated` false (no `git` key in the frontmatter yet): the legacy auto `- Branch:` /
 * `- Worktree:` lines are dropped first. → { text, changed }.
 */
export function mergeGit(resourcesText, item, { migrated = true, dismissed = [] } = {}) {
  const existing = String(resourcesText ?? '').trimEnd();
  const lines = existing ? existing.split('\n') : [];
  let changed = false;
  let at = -1;
  if (!migrated) {
    for (let i = lines.length - 1; i >= 0; i -= 1) {
      if (!LEGACY_GIT_RE.test(lines[i].trimEnd())) continue;
      lines.splice(i, 1);
      at = i;
      changed = true;
    }
  }
  const want = `- ${formatResource({ ...item, kind: 'Git' })}`;
  let first = -1;
  let auto = -1;
  lines.forEach((line, i) => {
    const b = BULLET_RE.exec(line);
    if (!b) return;
    const r = parseResourceLine(b[1]);
    if (r.kind !== 'Git') return;
    if (first < 0) first = i;
    if (auto < 0 && formatResource(r) === b[1].trim()) auto = i;
  });
  if (auto >= 0) {
    if (lines[auto] !== want) {
      lines[auto] = want;
      changed = true;
    }
  } else if (first < 0 && !dismissed.includes('git')) {
    lines.splice(at >= 0 ? at : 0, 0, want);
    changed = true;
  }
  return { text: lines.join('\n'), changed };
}

/** Keys that were in `before`'s Resources and are gone from `after`'s: what a human deleted. */
export function removedResourceKeys(before, after) {
  const now = new Set(after.resources.map((r) => r.key));
  return before.resources.map((r) => r.key).filter((k) => k && !now.has(k));
}

export function addDismissed(dismissed = [], keys = []) {
  const out = [...dismissed.filter((k) => !keys.includes(k)), ...keys];
  return out.slice(-MAX_DISMISSED);
}

// ------------------------------------------------------------------ model output

const MAX_SUMMARY_CHARS = 1200;
const MAX_TODOS = 20;

/**
 * Validate what the model answered: `## Summary` (required, prose, ≤ 1200 chars) and
 * optionally `## Todos` (checkbox items only; anything else is dropped; `## Plan` is read the
 * same). → { summary, todos|null } or null for garbage — the caller then keeps the old brief.
 */
export function parseModelOutput(text) {
  let t = String(text ?? '').trim();
  const fence = /^```[\w-]*\n([\s\S]*?)\n```$/.exec(t);
  if (fence) t = fence[1].trim();
  if (!/^##\s+summary\s*$/im.test(t)) return null;
  const b = parseBrief(t);
  const summary = b.summary.trim();
  if (!summary || summary.length > MAX_SUMMARY_CHARS || /^#/m.test(summary) || /^---$/m.test(summary)) return null;
  const hasTodos = /^##\s+(?:todos|plan)\s*$/im.test(t);
  const todos = hasTodos ? b.todos.filter((i) => i.text && i.text.length <= 300).slice(0, MAX_TODOS) : null;
  return { summary, todos: todos && todos.length ? todos : null };
}

// ------------------------------------------------------------------ continue prompt

/** The line that ends the handed-over brief in a continue prompt; the task for the new session follows it. */
export const CONTINUE_SEPARATOR = '---';
/** The task when nothing is written below the separator. */
export const CONTINUE_DEFAULT_TASK =
  'Pick up where it left off. Check the current state (git status, the PRs above) before changing anything, and tell me briefly what you found first.';

/** A resource as a continue-prompt bullet: `[label](url)`, a bare url, else its label / text. */
function promptLink(r) {
  if (r.url) return r.label && r.label !== r.url ? `[${r.label.replace(/[[\]]/g, '')}](${r.url})` : r.url;
  return r.label ?? r.text;
}

/**
 * The first prompt for a NEW session that picks up where the brief's session left off: where it
 * ran, the summary, where the work came from (Origin), its PRs and its linked worktree — not its
 * files, artifacts or todos — then a separator line the user writes the new session's task below.
 * `where` = { host, cwd } (defaults: the brief's frontmatter).
 */
export function continuePrompt(brief, where = {}) {
  const host = where.host ?? brief.meta?.host ?? null;
  const cwd = where.cwd ?? brief.meta?.cwd ?? null;
  const session = brief.meta?.session ?? null;
  const origin = [session ? `session ${session}` : 'a previous session', host ? `on ${host}` : null, cwd ? `in ${cwd}` : null]
    .filter(Boolean)
    .join(' ');
  const resources = brief.resources ?? [];
  const list = (kind) => resources.filter((r) => r.kind === kind).map((r) => `- ${promptLink(r)}`);
  const out = [`Continue the work of ${origin}. Its brief:`];
  if (brief.summary) out.push(brief.summary);
  const origins = list('Origin');
  if (origins.length) out.push(`Origin:\n${origins.join('\n')}`);
  const prs = list('PR');
  if (prs.length) out.push(`PRs:\n${prs.join('\n')}`);
  const wt = resources.find((r) => (r.kind === 'Git' && r.linked === true && r.path) || (r.kind === 'Worktree' && r.path));
  if (wt) out.push(`Worktree: \`${wt.path}\`${wt.kind === 'Git' && wt.branch ? ` (branch \`${wt.branch}\`)` : ''}`);
  out.push(CONTINUE_SEPARATOR);
  return out.join('\n\n');
}
