// Token auth (web.auth). Off by default: the server trusts the network it listens on (lib/guard.mjs).
// That trust ends where other people share the machine: anyone logged in to it reaches loopback,
// and with it every session this server can spawn into or type into. With web.auth.tokenFile set,
// every request needs the token, from loopback too:
//
// - `Authorization: Bearer <token>`: peers (hosts.<name>.webTokenFile), the operator's ./api, curl.
// - the `fleet_auth` cookie: a browser, after /login. It holds a value derived from the token
//   (HMAC), so the token itself never sits in the browser, and changing the token logs it out.
//
// `/api/health` stays open (the desktop app and scripts/deploy.sh probe it; it names the server and
// its version, nothing more). The token file must be readable by its owner only, like an ssh key.
import crypto from 'node:crypto';
import fs from 'node:fs';

export const COOKIE = 'fleet_auth';
export const MIN_TOKEN_LENGTH = 32;
const OPEN_PATHS = new Set(['/api/health', '/login', '/logout']);
const ONE_YEAR = 365 * 24 * 60 * 60;

/** Read a token file: trimmed, at least MIN_TOKEN_LENGTH characters, mode 600 (no group/other bits). */
export function readTokenFile(file, { fsImpl = fs, label = 'token file' } = {}) {
  let stat;
  try {
    stat = fsImpl.statSync(file);
  } catch (err) {
    throw new Error(`${label} ${file}: ${err.code === 'ENOENT' ? 'not found' : err.message}`);
  }
  if ((stat.mode & 0o077) !== 0) throw new Error(`${label} ${file} is readable by others: chmod 600 it`);
  const token = fsImpl.readFileSync(file, 'utf8').trim();
  if (token.length < MIN_TOKEN_LENGTH) throw new Error(`${label} ${file} must hold at least ${MIN_TOKEN_LENGTH} characters (e.g. openssl rand -hex 32)`);
  return token;
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

export function loginPage({ error = null } = {}) {
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
  <p>This server needs its access token (web.auth).</p>
  ${error ? `<p class="error">${escapeHtml(error)}</p>` : ''}
  <input name="token" type="password" autocomplete="current-password" placeholder="Token" autofocus required>
  <button type="submit">Sign in</button>
</form></body></html>
`;
}

/**
 * @param {object} opts
 * @param {string|null} opts.token the server's token; null → auth off (every request passes)
 * @param {(ms: number) => Promise<void>} [opts.delay] slows down a wrong login
 * @returns {{ enabled: boolean, check(req, url): null | { status, error, html? }, login(req, res, body): void, logout(req, res): void }}
 */
export function createAuth({ token = null, delay = (ms) => new Promise((r) => setTimeout(r, ms)) } = {}) {
  const enabled = typeof token === 'string' && token.length > 0;
  const expected = enabled ? cookieValue(token) : null;

  function authorized(req) {
    const header = String(req.headers?.authorization ?? '');
    const m = /^Bearer\s+(.+)$/i.exec(header);
    if (m && sameSecret(m[1].trim(), token)) return true;
    const c = cookiesOf(req)[COOKIE];
    return Boolean(c) && sameSecret(c, expected);
  }

  function secure(req) {
    return req.socket?.encrypted === true || String(req.headers?.['x-forwarded-proto'] ?? '').split(',')[0].trim() === 'https';
  }

  function setCookie(req, value, maxAge) {
    return `${COOKIE}=${value}; Path=/; HttpOnly; SameSite=Lax; Max-Age=${maxAge}${secure(req) ? '; Secure' : ''}`;
  }

  return {
    enabled,
    /** null when the request may go on; else what to answer (a page navigation gets the login page). */
    check(req, url) {
      if (!enabled || OPEN_PATHS.has(url.pathname) || authorized(req)) return null;
      const page = !url.pathname.startsWith('/api/') && (req.method === 'GET' || req.method === 'HEAD') && /text\/html/.test(String(req.headers?.accept ?? ''));
      return { status: 401, error: 'unauthorized (web.auth): send the token as a Bearer header or sign in at /login', page };
    },
    /** POST /login with `token=` (a form) or `{ "token": … }` (JSON). */
    async login(req, res, rawBody) {
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
      if (!enabled || !sameSecret(given.trim(), token)) {
        await delay(enabled ? 1000 : 0);
        const html = loginPage({ error: enabled ? 'Wrong token.' : 'This server has no web.auth.' });
        res.writeHead(401, { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store', 'content-length': Buffer.byteLength(html) });
        res.end(html);
        return;
      }
      res.writeHead(303, { location: '/', 'set-cookie': setCookie(req, expected, ONE_YEAR), 'cache-control': 'no-store' });
      res.end();
    },
    logout(req, res) {
      res.writeHead(303, { location: '/login', 'set-cookie': setCookie(req, '', 0), 'cache-control': 'no-store' });
      res.end();
    },
  };
}
