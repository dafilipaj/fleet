// Token auth (web.auth). Off by default: the server trusts the network it listens on (lib/guard.mjs).
// That trust ends where other people share the machine: anyone logged in to it reaches loopback,
// and with it every session this server can spawn into or type into. With web.auth.keychain set,
// every request needs the token, from loopback too:
//
// - `Authorization: Bearer <token>`: peers (hosts.<name>.webTokenKeychain), the operator's ./api, curl.
// - the `fleet_auth` cookie: a browser, after /login with the token, or with the password
//   (web.auth.passwordKeychain) for a phone, where pasting a 64-character token is a pain. The cookie
//   holds a value derived from the token (HMAC), so the token never sits in the browser, and a new
//   token signs every browser out.
//
// The token and the password come from the macOS Keychain (`security find-generic-password -s
// <service> -w`), or, where there is no Keychain (Linux), from a file only its owner can read
// (web.auth.tokenFile / passwordFile; a file wins over a Keychain item when both are set). Until the token can be read (a locked Keychain), the server
// fails closed: 503 for everything but /api/health. Wrong logins are throttled: after
// MAX_FAILURES in FAILURE_WINDOW_MS, /login answers 429 until the oldest failure ages out.
//
// `/api/health` stays open (the desktop app and scripts/deploy.sh probe it; it names the server and
// its version, nothing more).
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

export const COOKIE = 'fleet_auth';
export const MIN_TOKEN_LENGTH = 32;
export const MIN_PASSWORD_LENGTH = 8;
export const MAX_FAILURES = 5;
export const FAILURE_WINDOW_MS = 15 * 60 * 1000;
const OPEN_PATHS = new Set(['/api/health', '/login', '/logout']);
const ONE_YEAR = 365 * 24 * 60 * 60;
const MISS_TTL_MS = 30 * 1000;

/**
 * A Keychain item read on demand: `security find-generic-password -s <service> -w`. A hit is cached
 * for good, a miss (no item, a locked Keychain) is retried after 30 s. `minLength` rejects a value too
 * short to be what it claims (logged once).
 */
export function createKeychainSecret({ service, run, minLength = 1, now = Date.now, log = () => {} }) {
  let value = null;
  let missedAt = -Infinity;
  let warned = false;
  return async function get() {
    if (value) return value;
    if (!service || !run || now() - missedAt < MISS_TTL_MS) return null;
    try {
      const { stdout } = await run('security', ['find-generic-password', '-s', service, '-w'], { timeout: 5000 });
      const v = String(stdout ?? '').trim();
      if (v.length >= minLength) return (value = v);
      if (v && !warned) {
        warned = true;
        log(`[auth] Keychain item ${service} is shorter than ${minLength} characters: ignored`);
      }
    } catch {
      /* not in the Keychain, or it is locked */
    }
    missedAt = now();
    return null;
  };
}

/**
 * A secret kept in a file (`~/…` allowed), for hosts without a Keychain: read on demand, trimmed,
 * cached like a Keychain hit (a hit for good, a miss retried after 30 s). A file that its group or
 * others may read or write (mode & 0o077) is refused, never read (logged once), like a missing one.
 */
export function createFileSecret({ file, minLength = 1, now = Date.now, log = () => {}, fsImpl = fs, home = os.homedir() }) {
  const p = typeof file === 'string' && file.trim() ? path.resolve(file.trim() === '~' ? home : file.trim().startsWith('~/') ? path.join(home, file.trim().slice(2)) : file.trim()) : null;
  let value = null;
  let missedAt = -Infinity;
  const warned = new Set();
  const warn = (key, msg) => {
    if (warned.has(key)) return;
    warned.add(key);
    log(`[auth] ${msg}`);
  };
  return async function get() {
    if (value) return value;
    if (!p || now() - missedAt < MISS_TTL_MS) return null;
    try {
      const st = fsImpl.statSync(p);
      if ((st.mode & 0o077) !== 0) {
        warn('mode', `${p} is readable or writable by its group or others: ignored (chmod 600 it)`);
      } else {
        const v = String(fsImpl.readFileSync(p, 'utf8')).trim();
        if (v.length >= minLength) return (value = v);
        if (v) warn('short', `${p} holds a value shorter than ${minLength} characters: ignored`);
      }
    } catch {
      /* no such file (yet), or unreadable */
    }
    missedAt = now();
    return null;
  };
}

/** The secret source for a config entry: its file when one is set, else its Keychain item. */
export function createSecret({ file = null, service = null, run, minLength = 1, log = () => {}, now = Date.now, fsImpl = fs, home = os.homedir() }) {
  return file ? createFileSecret({ file, minLength, log, now, fsImpl, home }) : createKeychainSecret({ service, run, minLength, log, now });
}

function sameSecret(a, b) {
  const x = crypto.createHash('sha256').update(String(a)).digest();
  const y = crypto.createHash('sha256').update(String(b)).digest();
  return crypto.timingSafeEqual(x, y);
}

function cookieValue(token) {
  return crypto.createHmac('sha256', token).update('fleet-web-session').digest('hex');
}

function cookiesOf(req) {
  const out = {};
  for (const part of String(req.headers?.cookie ?? '').split(';')) {
    const i = part.indexOf('=');
    if (i > 0) out[part.slice(0, i).trim()] = part.slice(i + 1).trim();
  }
  return out;
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
}

export function loginPage({ error = null, password = false } = {}) {
  const what = password ? 'password or token' : 'token';
  return `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Fleet · sign in</title>
<style>
  :root { color-scheme: light dark; font-family: system-ui, sans-serif; }
  body { margin: 0; min-height: 100vh; display: grid; place-items: center; background: Canvas; color: CanvasText; }
  form { width: min(360px, calc(100vw - 32px)); display: grid; gap: 12px; }
  h1 { font-size: 18px; margin: 0; }
  p { margin: 0; font-size: 14px; opacity: .75; }
  input, button { font: inherit; padding: 10px 12px; border-radius: 8px; border: 1px solid GrayText; }
  button { cursor: pointer; }
  .error { color: #d33; opacity: 1; }
</style></head>
<body><form method="post" action="/login">
  <h1>Fleet</h1>
  <p>This server needs its ${what}.</p>
  ${error ? `<p class="error">${escapeHtml(error)}</p>` : ''}
  <input name="username" type="text" autocomplete="username" value="fleet" hidden>
  <input name="token" type="password" autocomplete="current-password" placeholder="${what[0].toUpperCase()}${what.slice(1)}" autofocus required>
  <button type="submit">Sign in</button>
</form></body></html>
`;
}

function sendHtml(res, status, html, headers = {}) {
  res.writeHead(status, { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store', 'content-length': Buffer.byteLength(html), ...headers });
  res.end(html);
}

/**
 * @param {object} opts
 * @param {boolean} opts.enabled web.auth is configured
 * @param {() => Promise<string|null>} opts.getToken the token (createKeychainSecret); null → not readable yet
 * @param {() => Promise<string|null>} [opts.getPassword] the phone password; null → token only
 * @param {(ms: number) => Promise<void>} [opts.delay] slows down a wrong login
 */
export function createAuth({ enabled = false, getToken = async () => null, getPassword = async () => null, now = Date.now, delay = (ms) => new Promise((r) => setTimeout(r, ms)), log = () => {} } = {}) {
  let failures = [];

  async function authorized(req, token) {
    const header = String(req.headers?.authorization ?? '');
    const m = /^Bearer\s+(.+)$/i.exec(header);
    if (m && sameSecret(m[1].trim(), token)) return true;
    const c = cookiesOf(req)[COOKIE];
    return Boolean(c) && sameSecret(c, cookieValue(token));
  }

  function secure(req) {
    return req.socket?.encrypted === true || String(req.headers?.['x-forwarded-proto'] ?? '').split(',')[0].trim() === 'https';
  }

  function setCookie(req, value, maxAge) {
    return `${COOKIE}=${value}; Path=/; HttpOnly; SameSite=Lax; Max-Age=${maxAge}${secure(req) ? '; Secure' : ''}`;
  }

  function lockedOut() {
    const t = now();
    failures = failures.filter((at) => t - at < FAILURE_WINDOW_MS);
    return failures.length >= MAX_FAILURES;
  }

  return {
    enabled,
    /** null when the request may go on; else what to answer (`page`: send a navigation to /login). */
    async check(req, url) {
      if (!enabled || url.pathname === '/api/health') return null;
      const token = await getToken();
      if (!token) return { status: 503, error: 'web.auth: the token is not readable from the Keychain (missing, or the Keychain is locked)', page: false };
      if (OPEN_PATHS.has(url.pathname) || (await authorized(req, token))) return null;
      const page = !url.pathname.startsWith('/api/') && (req.method === 'GET' || req.method === 'HEAD') && /text\/html/.test(String(req.headers?.accept ?? ''));
      return { status: 401, error: 'unauthorized (web.auth): send the token as a Bearer header or sign in at /login', page };
    },
    async loginForm(req, res) {
      sendHtml(res, 200, loginPage({ password: Boolean(await getPassword()) }));
    },
    /** POST /login with `token=` (a form; the token or the password) or `{ "token": … }` (JSON). */
    async login(req, res, rawBody) {
      const token = await getToken();
      const password = await getPassword();
      if (lockedOut()) {
        sendHtml(res, 429, loginPage({ error: 'Too many wrong attempts. Try again in a few minutes.', password: Boolean(password) }), { 'retry-after': String(Math.ceil(FAILURE_WINDOW_MS / 1000)) });
        return;
      }
      let given = '';
      if (/application\/json/.test(String(req.headers?.['content-type'] ?? ''))) {
        try {
          given = String(JSON.parse(rawBody || '{}')?.token ?? '');
        } catch {
          given = '';
        }
      } else {
        given = new URLSearchParams(rawBody).get('token') ?? '';
      }
      given = given.trim();
      const ok = Boolean(token) && Boolean(given) && (sameSecret(given, token) || (Boolean(password) && sameSecret(given, password)));
      if (!ok) {
        if (token) {
          failures.push(now());
          log(`[auth] wrong login from ${req.socket?.remoteAddress ?? '?'} (${failures.length} in the last ${FAILURE_WINDOW_MS / 60000} min)`);
          await delay(1000);
        }
        sendHtml(res, token ? 401 : 503, loginPage({ error: token ? 'Wrong password or token.' : 'The token is not readable from the Keychain.', password: Boolean(password) }));
        return;
      }
      res.writeHead(303, { location: '/', 'set-cookie': setCookie(req, cookieValue(token), ONE_YEAR), 'cache-control': 'no-store' });
      res.end();
    },
    logout(req, res) {
      res.writeHead(303, { location: '/login', 'set-cookie': setCookie(req, '', 0), 'cache-control': 'no-store' });
      res.end();
    },
  };
}
