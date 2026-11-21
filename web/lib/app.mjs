// HTTP server: /api/* goes to the API handler (lib/api.mjs), everything else is served
// as static files from `uiDir` (web/ui/dist, or config.web.ui).
import http from 'node:http';
import fs from 'node:fs';
import fsp from 'node:fs/promises';
import path from 'node:path';

import { HttpError, contentTypeFor, readBody, resolveStaticPath, sendJson } from './http.mjs';
import { BackendError } from './backends.mjs';

const PLACEHOLDER_HTML = `<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>fleet-web</title>
<style>body{font:16px -apple-system,system-ui,sans-serif;background:#111;color:#eee;
display:grid;place-items:center;height:100vh;margin:0}</style>
</head><body><p>fleet-web: the web UI is not built — run <code>fleet web build</code> (API is at /api/)</p></body></html>
`;

/** Does an If-None-Match header (possibly a list, possibly `*`) match `etag`? Weak comparison. */
export function etagMatches(header, etag) {
  if (typeof header !== 'string' || !header) return false;
  const strip = (t) => t.trim().replace(/^W\//, '');
  const want = strip(etag);
  return header.split(',').some((t) => t.trim() === '*' || strip(t) === want);
}

const IMMUTABLE = 'public, max-age=31536000, immutable';

/**
 * Cache policy for a static request path. Build tools emit content-hashed file names
 * under /assets/ (Vite: `index-R-dVrV7d.js`): a new build means a new name, so those can
 * be cached for good. Everything else (index.html, manifest, icons) is `no-cache`: always revalidated, cheap with the ETag → 304.
 */
export function cacheControlFor(urlPath) {
  return /^\/assets\/(?:[^/]+\/)*[^/]+-[A-Za-z0-9_-]{8,}\.[A-Za-z0-9]+$/.test(urlPath) ? IMMUTABLE : 'no-cache';
}

/** An API result that is a body stream (`files/raw`): headers first, then pipe; a client that goes away stops the read. */
function pipeResult(res, { status, headers = {}, stream }, logError, context) {
  return new Promise((resolve) => {
    res.writeHead(status, headers);
    const finish = () => {
      stream.destroy?.();
      resolve();
    };
    stream.on('error', (err) => {
      logError(err, context);
      res.destroy();
      finish();
    });
    res.on('close', finish);
    stream.pipe(res);
  });
}

export function createHttpServer({ handleApi, uiDir, guard = null, auth = null, log = () => {}, logError = () => {} }) {
  async function serveStatic(req, res, url) {
    if (!uiDir) {
      sendJson(res, 404, { error: 'not found' });
      return;
    }
    const resolved = resolveStaticPath(uiDir, url.pathname);
    if (!resolved.ok) {
      sendJson(res, resolved.status, { error: resolved.error });
      return;
    }

    let file = resolved.file;
    let stat = await fsp.stat(file).catch(() => null);
    if (stat?.isDirectory()) {
      file = path.join(file, 'index.html');
      stat = await fsp.stat(file).catch(() => null);
    }

    if (!stat?.isFile()) {
      if (url.pathname === '/' || url.pathname === '/index.html') {
        res.writeHead(200, {
          'content-type': 'text/html; charset=utf-8',
          'cache-control': 'no-cache',
          'content-length': Buffer.byteLength(PLACEHOLDER_HTML),
        });
        res.end(PLACEHOLDER_HTML);
        return;
      }
      sendJson(res, 404, { error: 'not found' });
      return;
    }

    // Weak validator from size + mtime: a reload revalidates (no-cache) and gets a 304.
    const etag = `W/"${stat.size.toString(16)}-${Math.floor(stat.mtimeMs).toString(16)}"`;
    const cacheControl = cacheControlFor(url.pathname);
    if (etagMatches(req.headers['if-none-match'], etag)) {
      res.writeHead(304, { etag, 'cache-control': cacheControl });
      res.end();
      return;
    }
    res.writeHead(200, {
      'content-type': contentTypeFor(file),
      'cache-control': cacheControl,
      etag,
      'content-length': stat.size,
    });
    if (req.method === 'HEAD') {
      res.end();
      return;
    }
    await new Promise((resolve) => {
      const stream = fs.createReadStream(file);
      stream.on('error', (err) => {
        logError(err, `static ${file}`);
        res.destroy();
        resolve();
      });
      stream.on('close', resolve);
      stream.pipe(res);
    });
  }

  const server = http.createServer((req, res) => {
    const startedAt = process.hrtime.bigint();
    let url;
    try {
      url = new URL(req.url, `http://${req.headers.host ?? 'localhost'}`);
    } catch {
      url = null;
    }

    const finish = (status) => {
      const ms = Number(process.hrtime.bigint() - startedAt) / 1e6;
      log(`${req.method} ${req.url} ${status} ${ms.toFixed(1)}ms`);
    };
    res.on('finish', () => finish(res.statusCode));
    res.on('close', () => {
      if (!res.writableEnded) finish(res.statusCode || 499);
    });

    (async () => {
      if (!url) {
        sendJson(res, 400, { error: 'bad request url' });
        return;
      }
      // Host / Origin checks (lib/guard.mjs): no DNS-rebound names, no cross-site requests.
      const denied = guard?.(req);
      if (denied) {
        log(`[guard] refused ${req.method} ${req.url}: ${denied.error}`);
        sendJson(res, denied.status, { error: denied.error });
        return;
      }
      // Token auth (lib/auth.mjs, web.auth): off unless configured.
      if (auth?.enabled) {
        if (url.pathname === '/login' && req.method === 'POST') {
          await auth.login(req, res, await readBody(req));
          return;
        }
        if (url.pathname === '/logout' && req.method === 'POST') {
          auth.logout(req, res);
          return;
        }
        const refused = await auth.check(req, url);
        if (!refused && url.pathname === '/login') {
          await auth.loginForm(req, res);
          return;
        }
        if (refused) {
          if (refused.page) {
            res.writeHead(302, { location: '/login', 'cache-control': 'no-store' });
            res.end();
          } else {
            // The header tells the UI that this server (not a peer it proxies) wants a sign-in.
            sendJson(res, refused.status, { error: refused.error }, refused.status === 401 ? { 'x-fleet-auth': 'login' } : {});
          }
          return;
        }
      }
      if (url.pathname.startsWith('/api/')) {
        const result = await handleApi(req, url);
        if (result?.stream) await pipeResult(res, result, logError, `${req.method} ${req.url}`);
        else if (result) sendJson(res, result.status, result.body);
        return;
      }
      if (req.method !== 'GET' && req.method !== 'HEAD') {
        sendJson(res, 405, { error: 'method not allowed' });
        return;
      }
      await serveStatic(req, res, url);
    })().catch((err) => {
      const status = err instanceof HttpError || err instanceof BackendError ? err.status : 500;
      if (status >= 500) logError(err, `${req.method} ${req.url}`);
      // An HttpError may carry extra fields (`body`, e.g. `noSpawn: true`); `error` is always its message.
      const extra = err instanceof HttpError && err.body && typeof err.body === 'object' ? err.body : {};
      if (!res.headersSent) sendJson(res, status, { ...extra, error: err?.message ?? 'internal error' });
      else res.destroy();
    });
  });

  server.headersTimeout = 30000;
  // Whole-request budget: generous enough for a large upload over a slow link (uploads are
  // capped by web.uploads.maxMB); headersTimeout still cuts off stalled clients early.
  server.requestTimeout = 15 * 60 * 1000;
  return server;
}
