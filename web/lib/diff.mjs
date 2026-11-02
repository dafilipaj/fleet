// The diff reviewer's source: what a session's worktree changed against a base, as structured
// files → hunks → lines. The working tree (tracked edits and untracked files alike) is captured
// as a git tree object through a throwaway index — a copy of the real one, so `git add -A` only
// rehashes what changed — and diffed against the merge-base of HEAD with the base branch.
// The tree id doubles as the fingerprint: identical content ⇔ identical id, so a poller sends
// `since=<tree>` and gets `{ unchanged: true }` back while nothing moved. Nothing in the repo
// changes: no refs, no real index; only loose blobs for changed files (git gc reclaims them).
import { createHash } from 'node:crypto';
import fsp from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';

/** git's empty tree: the base of a repo without commits. */
export const EMPTY_TREE = '4b825dc642cb6eb9a060e54bf8d69288fbee4904';
/** A file whose hunks run past this many lines is cut there (`truncated: true`). */
export const MAX_FILE_LINES = 5000;
/** The whole answer stops taking hunks past this many lines; later files come without them. */
export const MAX_TOTAL_LINES = 50000;
const GIT_TIMEOUT_MS = 20000;
/** Checkouts diffed for one session at most (its cwd's first). */
export const MAX_ROOTS = 12;
/** PlainLang specs: the diff carries their whole text, up to this size per side. */
export const MAX_PLAIN_BYTES = 512 * 1024;
const PLAIN_RE = /\.plain$/i;
const MAX_BUFFER = 64 * 1024 * 1024;
/** `?base=`: a branch, tag or sha; never an option. */
const REF_RE = /^(?!-)[A-Za-z0-9._/@{}~^-]{1,200}$/;
const TREE_RE = /^[0-9a-f]{40,64}$/;

export class DiffError extends Error {
  constructor(message, status) {
    super(message);
    this.status = status;
  }
}

export function validRef(ref) {
  return typeof ref === 'string' && REF_RE.test(ref) && !ref.includes('..');
}

export function validTree(sha) {
  return typeof sha === 'string' && TREE_RE.test(sha);
}

const STATUS = { A: 'added', D: 'deleted', M: 'modified', R: 'renamed', C: 'copied', T: 'modified' };

/** `git diff --name-status -z` → [{ status, path, oldPath }] in diff order. */
export function parseNameStatus(out) {
  const parts = out.split('\0');
  const files = [];
  for (let i = 0; i < parts.length; ) {
    const code = parts[i++];
    if (!code) continue;
    const kind = STATUS[code[0]] ?? 'modified';
    if (code[0] === 'R' || code[0] === 'C') {
      const oldPath = parts[i++];
      const newPath = parts[i++];
      files.push({ status: kind, path: newPath, oldPath });
    } else {
      const p = parts[i++];
      files.push({ status: kind, path: p, oldPath: kind === 'added' ? null : p });
    }
  }
  return files;
}

const HUNK_RE = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@ ?(.*)$/;

/**
 * A unified diff (`git diff -p`) → per file, in order: { binary, hunks: [{ header, oldStart,
 * oldLines, newStart, newLines, section, lines: [{ t: ' '|'+'|'-', o, n, text }] }] }, where
 * `o` / `n` are the old / new line numbers (null on the side a line is not on) and a
 * "\ No newline at end of file" marks the line before it `noEol: true`. Paths come from
 * `--name-status -z` (unambiguous), so the headers here only split the files.
 */
export function parsePatch(text) {
  const files = [];
  let file = null;
  let hunk = null;
  let o = 0;
  let n = 0;
  const lines = text.split('\n');
  if (lines.at(-1) === '') lines.pop();
  for (const line of lines) {
    if (line.startsWith('diff --git ') || line.startsWith('diff --cc ')) {
      file = { binary: false, hunks: [] };
      files.push(file);
      hunk = null;
      continue;
    }
    if (!file) continue;
    if (!hunk) {
      if (line.startsWith('Binary files ') || line === 'GIT binary patch') file.binary = true;
      const m = HUNK_RE.exec(line);
      if (!m) continue; // index / mode / rename / ---/+++ headers
    }
    const m = line.startsWith('@@') ? HUNK_RE.exec(line) : null;
    if (m) {
      hunk = {
        header: line,
        oldStart: Number(m[1]),
        oldLines: m[2] == null ? 1 : Number(m[2]),
        newStart: Number(m[3]),
        newLines: m[4] == null ? 1 : Number(m[4]),
        section: m[5] || '',
        lines: [],
      };
      file.hunks.push(hunk);
      o = hunk.oldStart;
      n = hunk.newStart;
      continue;
    }
    const t = line[0];
    if (t === ' ') hunk.lines.push({ t, o: o++, n: n++, text: line.slice(1) });
    else if (t === '-') hunk.lines.push({ t, o: o++, n: null, text: line.slice(1) });
    else if (t === '+') hunk.lines.push({ t, o: null, n: n++, text: line.slice(1) });
    else if (t === '\\') {
      const last = hunk.lines.at(-1);
      if (last) last.noEol = true;
    } else if (line === '') hunk.lines.push({ t: ' ', o: o++, n: n++, text: '' }); // a context line some tools strip
  }
  return files;
}

/**
 * @param {object} deps
 *   run      lib/run.mjs#run (git)
 *   tmpDir   where throwaway index files go (default os.tmpdir())
 *   home     never a checkout to diff ($HOME as a dotfiles repo)
 */
export function createDiffer({ run, tmpDir = os.tmpdir(), home = os.homedir() }) {
  const git = (cwd, args, opts = {}) =>
    run('git', ['-C', cwd, '-c', 'core.quotePath=false', ...args], { timeout: GIT_TIMEOUT_MS, maxBuffer: MAX_BUFFER, ...opts });

  async function revParse(cwd, args) {
    try {
      return (await git(cwd, ['rev-parse', ...args])).stdout.trim();
    } catch {
      return null;
    }
  }

  /** The repo's default branch: origin/HEAD, else a local main / master; null when none. */
  async function defaultBase(root) {
    const sym = await git(root, ['symbolic-ref', '--quiet', '--short', 'refs/remotes/origin/HEAD']).then(
      (r) => r.stdout.trim(),
      () => '',
    );
    if (sym) return sym;
    for (const b of ['main', 'master']) if (await revParse(root, ['--verify', '--quiet', `refs/heads/${b}^{commit}`])) return b;
    return null;
  }

  /** The working tree as a tree object: copy the index, `add -A` into the copy, write it. */
  async function worktreeTree(root) {
    const indexPath = path.resolve(root, (await revParse(root, ['--git-path', 'index'])) ?? '.git/index');
    const tmp = path.join(tmpDir, `fleet-diff-${process.pid}-${Math.random().toString(16).slice(2)}.index`);
    try {
      await fsp.copyFile(indexPath, tmp).catch((err) => {
        if (err?.code !== 'ENOENT') throw err; // a repo without an index yet: start empty
      });
      const env = { ...process.env, GIT_INDEX_FILE: tmp };
      await git(root, ['add', '-A', '--', '.'], { env });
      return (await git(root, ['write-tree'], { env })).stdout.trim();
    } finally {
      await fsp.rm(tmp, { force: true });
      await fsp.rm(`${tmp}.lock`, { force: true });
    }
  }

  /** Where a checkout stands: its working tree as a tree id, HEAD, the base → the fingerprint. */
  async function snapshot(root, base) {
    const tree = await worktreeTree(root);
    const head = await revParse(root, ['--verify', '--quiet', 'HEAD^{commit}']);
    const branchName = await git(root, ['symbolic-ref', '--quiet', '--short', 'HEAD']).then((r) => r.stdout.trim() || null, () => null);
    const baseRef = base ?? (await defaultBase(root));
    let baseSha = EMPTY_TREE;
    if (head) {
      baseSha = head;
      if (baseRef) {
        const mb = await git(root, ['merge-base', 'HEAD', baseRef]).then((r) => r.stdout.trim(), () => null);
        if (mb) baseSha = mb;
        else if (base != null) throw new DiffError(`no merge-base with ${base}`, 400);
      }
    }
    // The base can move too (a commit, a rebase): the fingerprint is both ids.
    return { root, branch: branchName, head, base: { ref: head ? baseRef : null, sha: baseSha }, tree, fingerprint: `${baseSha}:${tree}` };
  }

  /** A snapshot's files → hunks → lines. */
  async function filesOf(meta) {
    const { root, tree } = meta;
    const baseSha = meta.base.sha;
    const opts = ['--no-color', '--no-ext-diff', '-M', '--no-relative'];
    const [{ stdout: ns }, { stdout: patch }] = await Promise.all([
      git(root, ['diff', ...opts, '--name-status', '-z', baseSha, tree]),
      git(root, ['diff', ...opts, '-p', '-U3', baseSha, tree]),
    ]);
    const names = parseNameStatus(ns);
    const parsed = parsePatch(patch);
    let total = 0;
    let truncated = false;
    const stats = { files: names.length, additions: 0, deletions: 0 };
    const files = names.map((f, i) => {
      const p = parsed[i] ?? { binary: false, hunks: [] };
      let additions = 0;
      let deletions = 0;
      for (const h of p.hunks) for (const l of h.lines) l.t === '+' ? additions++ : l.t === '-' && deletions++;
      stats.additions += additions;
      stats.deletions += deletions;
      const size = p.hunks.reduce((s, h) => s + h.lines.length, 0);
      let hunks = p.hunks;
      let cut = false;
      if (total + size > MAX_TOTAL_LINES) {
        hunks = [];
        cut = true;
        truncated = true;
      } else if (size > MAX_FILE_LINES) {
        hunks = cutHunks(p.hunks, MAX_FILE_LINES);
        cut = true;
      }
      total += hunks.reduce((s, h) => s + h.lines.length, 0);
      return { ...f, binary: p.binary, additions, deletions, truncated: cut, hunks };
    });
    // A .plain spec is read as a whole (the UI's spec view diffs clauses, not lines): both texts ride along.
    await mapLimit(files.filter((f) => !f.binary && PLAIN_RE.test(f.path)), 4, async (f) => {
      const blob = (rev, p) =>
        git(root, ['cat-file', 'blob', `${rev}:${p}`]).then(
          (r) => (r.stdout.length > MAX_PLAIN_BYTES ? null : r.stdout),
          () => null,
        );
      const [before, after] = await Promise.all([
        f.status === 'added' || baseSha === EMPTY_TREE ? null : blob(baseSha, f.oldPath ?? f.path),
        f.status === 'deleted' ? null : blob(tree, f.path),
      ]);
      f.plain = { old: before, new: after };
    });
    return { files, stats, truncated };
  }


  /**
   * The diff for a directory inside a checkout.
   *   cwd     the session's working directory
   *   base    a ref to diff against (default: the repo's default branch); the diff starts at
   *           the merge-base of HEAD with it, so commits on the branch count as changes
   *   since   a fingerprint from an earlier answer: unchanged → { unchanged: true } without files
   * → { root, branch, head, base: { ref, sha }, tree, fingerprint, unchanged, files, stats, truncated }
   */
  async function diff(cwd, { base = null, since = null } = {}) {
    if (base != null && !validRef(base)) throw new DiffError('invalid base ref', 400);
    const root = await revParse(cwd, ['--show-toplevel']);
    if (!root) throw new DiffError('not a git repository', 409);
    const meta = await snapshot(root, base);
    if (since && since === meta.fingerprint) return { ...meta, unchanged: true };
    return { ...meta, unchanged: false, ...(await filesOf(meta)) };
  }

  const toplevels = new Map(); // a found checkout dir → git's toplevel (realpath), or null

  /** The nearest directory holding a `.git` (dir: a main checkout, file: a linked worktree). */
  async function nearestCheckout(dir, memo) {
    const trail = [];
    let d = dir;
    let found = null;
    for (;;) {
      if (memo.has(d)) {
        found = memo.get(d);
        break;
      }
      trail.push(d);
      if (await fsp.stat(path.join(d, '.git')).then(() => true, () => false)) {
        found = d;
        break;
      }
      const up = path.dirname(d);
      if (up === d) break;
      d = up;
    }
    for (const t of trail) memo.set(t, found);
    return found;
  }

  /**
   * The checkouts a session works in: its cwd's first, then those holding the files it worked
   * on (`worked`, most recent first) — a session in a meta repo edits many worktrees it never
   * cd's into. Never $HOME or `/` (a dotfiles repo would mean hashing the whole home).
   */
  async function roots(cwd, worked = []) {
    const out = [];
    const add = (r) => {
      if (r && !out.includes(r) && r !== home && r !== '/') out.push(r);
    };
    if (cwd) add(await revParse(cwd, ['--show-toplevel']));
    const memo = new Map();
    for (const p of worked) {
      if (out.length >= MAX_ROOTS) break;
      if (typeof p !== 'string' || !path.isAbsolute(p)) continue;
      const dir = await nearestCheckout(path.dirname(p), memo);
      if (!dir || dir === home) continue;
      if (!toplevels.has(dir)) toplevels.set(dir, await revParse(dir, ['--show-toplevel']));
      add(toplevels.get(dir));
    }
    return out;
  }

  /**
   * Every checkout's diff in one answer. `since`: the combined fingerprint of an earlier one —
   * unchanged → { unchanged: true } without repos. A checkout that fails (deleted worktree)
   * comes back as { root, error }.
   */
  async function diffAll(rootList, { since = null } = {}) {
    const metas = await mapLimit(rootList, 4, (root) => snapshot(root, null).catch((err) => ({ root, error: err.message || String(err) })));
    const fingerprint = createHash('sha1')
      .update(metas.map((m) => `${m.root}=${m.fingerprint ?? 'error'}`).join('\n'))
      .digest('hex');
    if (since && since === fingerprint) return { fingerprint, unchanged: true };
    const repos = await mapLimit(metas, 4, async (m) => (m.error ? m : { ...m, ...(await filesOf(m).catch((err) => ({ error: err.message || String(err), files: [] }))) }));
    return { fingerprint, unchanged: false, repos };
  }

  return { diff, roots, diffAll };
}

function cutHunks(hunks, max) {
  const out = [];
  let left = max;
  for (const h of hunks) {
    if (left <= 0) break;
    out.push(h.lines.length <= left ? h : { ...h, lines: h.lines.slice(0, left) });
    left -= h.lines.length;
  }
  return out;
}

async function mapLimit(items, limit, fn) {
  const out = new Array(items.length);
  let next = 0;
  const worker = async () => {
    while (next < items.length) {
      const i = next++;
      out[i] = await fn(items[i], i);
    }
  };
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, worker));
  return out;
}
