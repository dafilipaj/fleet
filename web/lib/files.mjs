// Files an agent mentions in chat ("Updated docs/setup.md:12"): stat them (so the UI links
// only the ones that exist), stream one for the in-app preview, or open it on this host.
// Paths resolve against the session's cwd; a relative path missing there falls back to files
// the session touched (lib/touched.mjs: "Updated docs/x.md" after editing /…/other-repo/docs/x.md),
// then to their ancestor dirs and the configured `web.files.roots`. Whatever the route, after
// realpath the file must stay inside $HOME or that cwd (symlinks out are refused), and a few
// secret stores are never served.
import fs from 'node:fs';
import fsp from 'node:fs/promises';
import path from 'node:path';

import { HttpError } from './http.mjs';

export const MAX_STAT_PATHS = 200;
export const MAX_PATH_LENGTH = 4096;
/** Text / markdown larger than this is not previewed (download still works). */
export const PREVIEW_MAX_BYTES = 5 * 1024 * 1024;
const SNIFF_BYTES = 4096;
const STAT_TTL_MS = 5000;
const STAT_CACHE_MAX = 2000;
/** Fallback bounds for a relative path missing under the cwd. */
const MAX_TOUCHED_MATCHES = 20; // touched files ending in /<rel> that are checked
const MAX_TOUCHED_SCAN = 500; // touched files whose ancestors become candidate roots
const MAX_ANCESTOR_DEPTH = 8;
const MAX_CANDIDATE_ROOTS = 40;

const MARKDOWN_EXT = new Set(['.md', '.markdown', '.mdx', '.mdown', '.mkd']);
const IMAGE_TYPES = {
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.jpeg': 'image/jpeg',
  '.gif': 'image/gif',
  '.webp': 'image/webp',
  '.avif': 'image/avif',
  '.bmp': 'image/bmp',
  '.ico': 'image/x-icon',
  '.svg': 'image/svg+xml',
};
const OTHER_TYPES = {
  '.pdf': 'application/pdf',
  '.zip': 'application/zip',
  '.gz': 'application/gzip',
  '.mp4': 'video/mp4',
  '.mov': 'video/quicktime',
  '.mp3': 'audio/mpeg',
  '.wav': 'audio/wav',
};
// Known binary formats: never sniffed as text.
const BINARY_EXT = new Set(['.zip', '.gz', '.tgz', '.bz2', '.xz', '.7z', '.tar', '.dmg', '.pkg', '.iso', '.exe', '.dll', '.so', '.dylib', '.o', '.a', '.class', '.jar', '.wasm', '.mp4', '.mov', '.mp3', '.wav', '.woff', '.woff2', '.ttf', '.otf', '.sqlite', '.db', '.psd', '.heic', '.tiff', '.tif', '.docx', '.xlsx', '.pptx', '.key', '.numbers', '.pages']);

// Secret stores under $HOME that are never served, even though they are inside it.
const DENY_DIRS = ['.ssh', '.gnupg', '.aws', '.kube', '.docker', '.password-store', '.config/gh', 'Library/Keychains'];
const DENY_NAMES = /^(?:\.envrc|\.netrc|\.pgpass|\.env(?:\..+)?)$/;

// Opening these would run something (or install it) rather than show it: reveal instead.
const RUNNABLE_EXT = new Set(['.app', '.command', '.tool', '.terminal', '.sh', '.bash', '.zsh', '.fish', '.csh', '.ksh', '.py', '.rb', '.pl', '.php', '.js', '.mjs', '.scpt', '.scptd', '.applescript', '.workflow', '.action', '.pkg', '.mpkg', '.dmg', '.jar', '.exe', '.bat', '.cmd', '.ps1', '.desktop', '.webloc', '.inetloc', '.fileloc', '.url', '.shortcut', '.osax', '.prefpane', '.kext', '.saver', '.mobileconfig']);

/**
 * Split a mention into the path and an optional position: `a/b.md:12`, `a/b.md:12:3`,
 * `a/b.md#L12` (also `#L12-L20`). → { path, line?, col? }
 */
export function parsePathSpec(raw) {
  let p = String(raw ?? '').trim();
  let line;
  let col;
  const gh = /#L(\d+)(?:-L?\d+)?$/.exec(p);
  const colon = /:(\d+)(?::(\d+))?$/.exec(p);
  if (gh && gh.index > 0) {
    line = Number(gh[1]);
    p = p.slice(0, gh.index);
  } else if (colon && colon.index > 0) {
    line = Number(colon[1]);
    if (colon[2] !== undefined) col = Number(colon[2]);
    p = p.slice(0, colon.index);
  }
  return { path: p, ...(line ? { line } : {}), ...(col ? { col } : {}) };
}

/** `~`, `~/x`, absolute, or relative to `cwd` → a normalized absolute path (null when unusable). */
export function resolveUserPath(p, { cwd, home }) {
  if (typeof p !== 'string' || !p || p.length > MAX_PATH_LENGTH || p.includes('\0')) return null;
  if (p === '~') return path.resolve(home);
  if (p.startsWith('~/')) return path.resolve(home, p.slice(2));
  if (p.startsWith('~')) return null; // ~otheruser
  if (path.isAbsolute(p)) return path.resolve(p);
  return path.resolve(cwd || home, p);
}

const inside = (p, root) => p === root || p.startsWith(root.endsWith(path.sep) ? root : root + path.sep);

/** Is `p` (already real) one of the secret stores under `home`? */
export function isDenied(p, home) {
  if (DENY_NAMES.test(path.basename(p))) return true;
  return DENY_DIRS.some((d) => inside(p, path.join(home, d)));
}

/** At most this many modules in a files/plain answer. */
const MAX_PLAIN_MODULES = 2000;

/** Lowercased extension, `''` for none. */
const extOf = (p) => path.extname(p).toLowerCase();

/** The preview kind from the name alone: markdown | image | pdf | text | other | null (sniff). */
export function kindByName(p) {
  const ext = extOf(p);
  if (MARKDOWN_EXT.has(ext)) return 'markdown';
  if (IMAGE_TYPES[ext]) return 'image';
  if (ext === '.pdf') return 'pdf';
  if (BINARY_EXT.has(ext)) return 'other';
  return null;
}

/** A few KB decide text vs binary: no NUL bytes and valid UTF-8 (a cut-off last char is fine). */
export function looksLikeText(buf) {
  if (!buf || buf.length === 0) return true;
  if (buf.includes(0)) return false;
  try {
    new TextDecoder('utf-8', { fatal: true }).decode(buf);
    return true;
  } catch {
    // The sniff window may end mid-character: retry without the last 3 bytes.
    if (buf.length < 4) return false;
    try {
      new TextDecoder('utf-8', { fatal: true }).decode(buf.subarray(0, buf.length - 3));
      return true;
    } catch {
      return false;
    }
  }
}

async function sniff(file, size) {
  if (size === 0) return Buffer.alloc(0);
  const fh = await fsp.open(file, 'r');
  try {
    const buf = Buffer.alloc(Math.min(SNIFF_BYTES, size));
    const { bytesRead } = await fh.read(buf, 0, buf.length, 0);
    return buf.subarray(0, bytesRead);
  } finally {
    await fh.close().catch(() => {});
  }
}

/** Content-Type for the raw endpoint. Text of any kind is served as text/plain (never rendered). */
export function contentTypeForKind(kind, file) {
  if (kind === 'markdown' || kind === 'text') return 'text/plain; charset=utf-8';
  if (kind === 'image') return IMAGE_TYPES[extOf(file)] ?? 'application/octet-stream';
  if (kind === 'pdf') return 'application/pdf';
  return OTHER_TYPES[extOf(file)] ?? 'application/octet-stream';
}

function contentDisposition(type, name) {
  const ascii = name.replace(/[^\x20-\x7e]/g, '_').replace(/["\\]/g, '_');
  return `${type}; filename="${ascii}"; filename*=UTF-8''${encodeURIComponent(name)}`;
}

/** `input` relative to the cwd when inside it, else `~/…` under home, else absolute. */
function displayPath(abs, { cwd, home }) {
  if (cwd && inside(abs, cwd) && abs !== cwd) return path.relative(cwd, abs);
  if (inside(abs, home)) return abs === home ? '~' : `~/${path.relative(home, abs)}`;
  return abs;
}

/**
 * @param {object} opts
 *   home       this host's $HOME
 *   run        lib/run.mjs (for `open`)
 *   platform   process.platform
 *   hasBinary  (name) → boolean, for xdg-open (default: PATH lookup)
 *   touched    lib/touched.mjs index ({ get(session) }) — optional; enables the fallbacks
 *   roots      extra absolute dirs a relative path may be under (`web.files.roots`); they only
 *              add candidates — the sandbox stays $HOME + cwd
 */
export function createFiles({ home, run, platform = process.platform, hasBinary = pathHas, now = Date.now, touched = null, roots: extraRoots = [] } = {}) {
  const realHome = safeRealpathSync(home) ?? path.resolve(home);
  const cache = new Map(); // `${session}\0${touched version}\0${cwd}\0${input}` → { at, entry }

  async function roots(cwd) {
    const out = [realHome];
    if (cwd) {
      const real = await fsp.realpath(cwd).catch(() => null);
      if (real && !out.includes(real)) out.push(real);
    }
    return out;
  }

  /** The session's touched paths ({ version, paths }) or null — never throws. */
  async function touchedFor(session) {
    if (!touched || !session) return null;
    try {
      return await touched.get(session);
    } catch {
      return null;
    }
  }

  /**
   * Resolve + check one mention. → { input, path, rel, line?, col?, abs, real?, stat?, error?, status?, resolvedVia? }
   * `status` 403 = outside the allowed roots / a secret store, 404 = missing, 400 = unusable.
   * `resolvedVia` (found only): cwd (as written: relative to the cwd, `~/…` or absolute),
   * touched (a file the session touched ends in /<path>) or root (<candidate root>/<path>).
   * `touchedPaths` — touchedFor(session), passed in so a batch reads it once.
   */
  async function locate(input, cwd, touchedPaths = null) {
    const spec = parsePathSpec(input);
    const base = { input, ...(spec.line ? { line: spec.line } : {}), ...(spec.col ? { col: spec.col } : {}) };
    const abs = resolveUserPath(spec.path, { cwd, home: realHome });
    if (!abs) return { ...base, path: spec.path, status: 400, error: 'bad path' };
    const allowed = await roots(cwd);
    const direct = await check(abs, base, cwd, allowed);
    if (direct.stat) return { ...direct, resolvedVia: 'cwd' };
    if (direct.status !== 404 || !touchedPaths) return direct;
    const p = spec.path;
    if (p.startsWith('~') || path.isAbsolute(p)) return direct;
    const rel = path.normalize(p).replace(/\/+$/, '');
    if (!rel || rel === '.' || rel === '..' || rel.startsWith(`..${path.sep}`)) return direct;
    for (const cand of candidates(rel, touchedPaths, cwd)) {
      const hit = await check(cand.abs, base, cwd, allowed);
      if (hit.stat) return { ...hit, resolvedVia: cand.via };
    }
    return direct;
  }

  /** Fallback locations for a relative `rel`, best first: touched files, then candidate roots. */
  function* candidates(rel, touchedPaths, cwd) {
    const suffix = `${path.sep}${rel}`;
    let matches = 0;
    for (const t of touchedPaths.paths) {
      if (!t.endsWith(suffix)) continue;
      yield { abs: t, via: 'touched' };
      if (++matches >= MAX_TOUCHED_MATCHES) break;
    }
    const seen = new Set(cwd ? [path.resolve(cwd)] : []);
    const dirs = [];
    const scan = touchedPaths.paths.slice(0, MAX_TOUCHED_SCAN);
    // Ancestors of touched files up to (not including) $HOME, nearest first, most recent file first.
    outer: for (const t of scan) {
      let d = path.dirname(t);
      for (let depth = 0; depth < MAX_ANCESTOR_DEPTH; depth++) {
        if (d === realHome || d === path.dirname(d) || !inside(d, realHome)) break;
        if (!seen.has(d)) {
          seen.add(d);
          dirs.push(d);
          if (dirs.length >= MAX_CANDIDATE_ROOTS) break outer;
        }
        d = path.dirname(d);
      }
    }
    for (const r of extraRoots) {
      const d = path.resolve(r);
      if (!seen.has(d)) {
        seen.add(d);
        dirs.push(d);
      }
    }
    for (const d of dirs) yield { abs: path.join(d, rel), via: 'root' };
  }

  /** The sandbox + existence check of one absolute candidate. */
  async function check(abs, base, cwd, allowed) {
    const out = { ...base, path: abs, rel: displayPath(abs, { cwd, home: realHome }) };
    // Lexical check first (cheap, and never stats anything outside), then the real path.
    const lexicalRoots = [path.resolve(home), realHome, ...(cwd ? [path.resolve(cwd)] : []), ...allowed];
    if (!lexicalRoots.some((r) => inside(abs, r))) return { ...out, status: 403, error: 'outside the allowed directories' };
    let real;
    try {
      real = await fsp.realpath(abs);
    } catch (err) {
      if (err?.code === 'ENOENT' || err?.code === 'ENOTDIR') return { ...out, status: 404, error: 'not found' };
      return { ...out, status: 403, error: err?.code === 'EACCES' ? 'permission denied' : 'not accessible' };
    }
    if (!allowed.some((r) => inside(real, r))) return { ...out, status: 403, error: 'outside the allowed directories' };
    if (isDenied(real, realHome) || isDenied(abs, realHome)) return { ...out, status: 403, error: 'not served (secrets)' };
    let st;
    try {
      st = await fsp.stat(real);
    } catch {
      return { ...out, status: 404, error: 'not found' };
    }
    return { ...out, real, stat: st };
  }

  async function kindOf(file, st) {
    const byName = kindByName(file);
    if (byName === 'image' || byName === 'pdf' || byName === 'other') return byName;
    try {
      return looksLikeText(await sniff(file, st.size)) ? (byName ?? 'text') : 'other';
    } catch {
      return byName ?? 'other';
    }
  }

  async function statOne(input, cwd, session, touchedPaths) {
    const key = `${session?.session_id ?? ''}\0${touchedPaths?.version ?? ''}\0${cwd}\0${input}`;
    const hit = cache.get(key);
    if (hit && now() - hit.at < STAT_TTL_MS) return hit.entry;
    const loc = await locate(input, cwd, touchedPaths);
    let entry;
    if (!loc.stat) {
      entry = {
        input,
        path: loc.path,
        ...(loc.rel ? { rel: loc.rel } : {}),
        ...(loc.line ? { line: loc.line } : {}),
        ...(loc.col ? { col: loc.col } : {}),
        exists: false,
        ...(loc.status === 403 ? { forbidden: true } : {}),
      };
    } else {
      const isFile = loc.stat.isFile();
      entry = {
        input,
        path: loc.path,
        rel: loc.rel,
        ...(loc.line ? { line: loc.line } : {}),
        ...(loc.col ? { col: loc.col } : {}),
        exists: true,
        resolvedVia: loc.resolvedVia,
        isFile,
        isDir: loc.stat.isDirectory(),
        size: loc.stat.size,
        mtime: Math.round(loc.stat.mtimeMs),
        kind: isFile ? await kindOf(loc.real, loc.stat) : 'other',
      };
    }
    if (cache.size >= STAT_CACHE_MAX) cache.clear();
    cache.set(key, { at: now(), entry });
    return entry;
  }

  /** POST …/files/stat — `paths` from the client, deduplicated, capped. `session` enables the fallbacks. */
  async function stat(paths, cwd, { session = null } = {}) {
    if (!Array.isArray(paths)) throw new HttpError('paths must be an array of strings', 400);
    if (paths.length > MAX_STAT_PATHS) throw new HttpError(`too many paths (max ${MAX_STAT_PATHS})`, 400);
    const unique = [...new Set(paths.filter((p) => typeof p === 'string' && p.trim() && p.length <= MAX_PATH_LENGTH))];
    const touchedPaths = await touchedFor(session);
    const files = await Promise.all(unique.map((p) => statOne(p, cwd, session, touchedPaths)));
    return { cwd: cwd ?? null, home: realHome, files };
  }

  /** Locate a file for raw/open; throws HttpError 400/403/404. */
  async function requireFile(input, cwd, session) {
    if (typeof input !== 'string' || !input.trim()) throw new HttpError('path is required', 400);
    const loc = await locate(input.trim(), cwd, await touchedFor(session));
    if (!loc.stat) throw new HttpError(`${loc.error}: ${loc.path}`, loc.status ?? 404);
    return loc;
  }

  /**
   * GET …/files/raw — { status, headers, stream } (the app pipes `stream` to the response).
   * Inline text/markdown over PREVIEW_MAX_BYTES → 413; `download` → attachment, no cap.
   */
  async function raw(input, cwd, { download = false, session = null } = {}) {
    const loc = await requireFile(input, cwd, session);
    if (!loc.stat.isFile()) throw new HttpError(`not a file: ${loc.path}`, 400);
    const kind = await kindOf(loc.real, loc.stat);
    if (!download && (kind === 'text' || kind === 'markdown') && loc.stat.size > PREVIEW_MAX_BYTES) {
      throw new HttpError(`too large to preview (${loc.stat.size} bytes, max ${PREVIEW_MAX_BYTES}) — download it instead`, 413);
    }
    const name = path.basename(loc.path);
    const headers = {
      'content-type': download ? (OTHER_TYPES[extOf(name)] ?? IMAGE_TYPES[extOf(name)] ?? 'application/octet-stream') : contentTypeForKind(kind, name),
      'content-length': loc.stat.size,
      'content-disposition': contentDisposition(download ? 'attachment' : 'inline', name),
      'last-modified': new Date(loc.stat.mtimeMs).toUTCString(),
      'cache-control': 'no-store',
      'x-content-type-options': 'nosniff',
      'x-fleet-kind': kind,
    };
    // A file opened straight in a tab (an SVG, say) must never run script on this origin.
    // Not for PDFs: Chrome refuses to show a PDF under a sandbox CSP.
    if (kind !== 'pdf') headers['content-security-policy'] = "sandbox; default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'";
    return { status: 200, headers, stream: fs.createReadStream(loc.real) };
  }

  /** The command `open` runs: [bin, args] — or null when the host has no opener. */
  function openCommand(file, { reveal }) {
    if (platform === 'darwin') return ['open', reveal ? ['-R', file] : [file]];
    if (!hasBinary('xdg-open')) return null;
    return ['xdg-open', [reveal ? path.dirname(file) : file]];
  }

  /**
   * POST …/files/open — open the file with its default app on THIS host. Anything that would
   * run (executable bit, .command/.app/.sh/…) is revealed in its folder instead.
   */
  async function open(input, cwd, { session = null } = {}) {
    const loc = await requireFile(input, cwd, session);
    const isDir = loc.stat.isDirectory();
    const runnable = isDir ? extOf(loc.real) === '.app' : (loc.stat.mode & 0o111) !== 0 || RUNNABLE_EXT.has(extOf(loc.real));
    const cmd = openCommand(loc.real, { reveal: runnable });
    if (!cmd) throw new HttpError('no opener on this host (needs macOS `open` or `xdg-open`)', 501);
    try {
      await run(cmd[0], cmd[1], { timeout: 10000 });
    } catch (err) {
      throw new HttpError(`open failed: ${err?.message ?? err}`, 502);
    }
    return { ok: true, path: loc.path, revealed: runnable, command: cmd[0] };
  }

  /**
   * POST …/files/plain — the PlainLang modules beside a .plain file, so a term can be looked up
   * in the modules it imports and the rest of its repo: every `*.plain` of the file's git checkout
   * (tracked + untracked, not ignored), else of its directory. → { root, files: [abs] }
   */
  async function plainModules(input, cwd, { session = null } = {}) {
    const loc = await requireFile(input, cwd, session);
    const dir = path.dirname(loc.real);
    let root = dir;
    let list = null;
    try {
      root = (await run('git', ['-C', dir, 'rev-parse', '--show-toplevel'], { timeout: 5000 })).stdout.trim() || dir;
      const { stdout } = await run('git', ['-C', root, 'ls-files', '-z', '--cached', '--others', '--exclude-standard', '--', '*.plain'], { timeout: 5000, maxBuffer: 8 * 1024 * 1024 });
      list = [...new Set(stdout.split('\0').filter(Boolean))].map((rel) => path.join(root, rel));
    } catch {
      root = dir;
      list = (await fsp.readdir(dir).catch(() => [])).filter((n) => n.endsWith('.plain')).map((n) => path.join(dir, n));
    }
    const files = list.filter((f) => !isDenied(f, realHome)).sort().slice(0, MAX_PLAIN_MODULES);
    return { root, files, truncated: list.length > files.length };
  }

  return { stat, raw, open, locate, plainModules };
}

function safeRealpathSync(p) {
  try {
    return fs.realpathSync(p);
  } catch {
    return null;
  }
}

function pathHas(name) {
  return (process.env.PATH ?? '').split(path.delimiter).some((d) => {
    if (!d) return false;
    try {
      fs.accessSync(path.join(d, name), fs.constants.X_OK);
      return true;
    } catch {
      return false;
    }
  });
}
