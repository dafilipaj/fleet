import http from 'node:http';
import https from 'node:https';

import { sortSessions } from './util.mjs';

// Peer origin → HTTP proxy url, for peers on another network (hosts.<name>.webProxy): set once from
// the config. Node's built-in proxy support (agent `proxyEnv`, Node >= 24.5) does the tunnelling.
let peerProxies = {};
const agents = new Map();
// Peer origin → the host[:port] to dial instead of the url's own (hosts.<name>.webConnect), for a
// name the proxy cannot resolve: TLS (SNI, the certificate check) and Host stay the url's name.
let peerConnects = {};

export function setPeerProxies(map) {
  peerProxies = map && typeof map === 'object' ? { ...map } : {};
  agents.clear();
}

export function setPeerConnects(map) {
  peerConnects = map && typeof map === 'object' ? { ...map } : {};
}

// The url to dial and the request options that keep the original name for TLS and Host.
function dialTarget(target) {
  const connect = peerConnects[target.origin];
  if (!connect) return { url: target, options: {} };
  const url = new URL(target.href);
  const at = new URL(`${target.protocol}//${connect}`);
  url.hostname = at.hostname;
  url.port = at.port || target.port;
  const options = { headers: { host: target.host } };
  if (target.protocol === 'https:') options.servername = target.hostname;
  return { url, options };
}

function proxyAgentFor(target) {
  const proxy = peerProxies[target.origin];
  if (!proxy) return undefined;
  const key = `${target.protocol}${proxy}`;
  if (!agents.has(key)) {
    const Agent = target.protocol === 'https:' ? https.Agent : http.Agent;
    agents.set(key, new Agent({ keepAlive: true, proxyEnv: { HTTP_PROXY: proxy, HTTPS_PROXY: proxy } }));
  }
  return agents.get(key);
}

// fetch() has no per-request proxy without undici, so a proxied peer goes through http(s).request
// and comes back as a Response, which is all the callers use.
function requestViaAgent(target, { method = 'GET', headers = {}, body = null, signal } = {}, agent) {
  const { url, options } = dialTarget(target);
  return new Promise((resolve, reject) => {
    const out = (target.protocol === 'https:' ? https : http).request(url, { ...options, method, headers: { ...headers, ...options.headers }, agent, signal }, (res) => {
      const chunks = [];
      res.on('data', (c) => chunks.push(c));
      res.on('error', reject);
      res.on('end', () => {
        const h = new Headers();
        for (const [k, v] of Object.entries(res.headers)) if (v != null) h.set(k, Array.isArray(v) ? v.join(', ') : String(v));
        const status = res.statusCode ?? 502;
        const noBody = status === 204 || status === 304;
        resolve(new Response(noBody ? null : Buffer.concat(chunks), { status, headers: h }));
      });
    });
    out.on('error', reject);
    out.end(body ?? undefined);
  });
}

function withLocalFlag(search) {
  const params = new URLSearchParams(search ?? '');
  params.set('local', '1');
  return params.toString();
}

async function fetchWithTimeout(url, opts, timeoutMs) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const target = new URL(url);
    const agent = proxyAgentFor(target);
    if (agent || peerConnects[target.origin]) return await requestViaAgent(target, { ...opts, signal: controller.signal }, agent);
    return await fetch(url, { ...opts, signal: controller.signal });
  } finally {
    clearTimeout(timer);
  }
}

function describeError(err, timeoutMs) {
  if (err?.name === 'AbortError' || err?.name === 'TimeoutError') return `timed out after ${timeoutMs}ms`;
  const cause = err?.cause?.message ?? err?.cause?.code;
  return cause ? `${err.message} (${cause})` : String(err?.message ?? err);
}

/**
 * Ask a peer for its local fleet and return a single host entry for it.
 * Never throws — an unreachable peer becomes { ok:false, error:"unreachable: ..." }.
 */
export async function fetchPeerHost(name, baseUrl, { timeoutMs = 6000, now = Date.now } = {}) {
  const fetchedAt = now();
  try {
    const res = await fetchWithTimeout(
      `${baseUrl}/api/fleet?local=1`,
      { headers: { accept: 'application/json' } },
      timeoutMs,
    );
    if (!res.ok) {
      return { name, ok: false, error: `unreachable: HTTP ${res.status}`, fetchedAt, sessions: [] };
    }
    const body = await res.json();
    const hosts = Array.isArray(body?.hosts) ? body.hosts : [];
    const host = hosts.find((h) => h?.name === name) ?? hosts[0];
    if (!host) {
      return { name, ok: false, error: 'unreachable: peer returned no hosts', fetchedAt, sessions: [] };
    }
    const sessions = sortSessions(Array.isArray(host.sessions) ? host.sessions : []).map((s) => ({
      ...s,
      host: name,
    }));
    return {
      name,
      ok: host.ok !== false,
      ...(host.ok === false && host.error ? { error: host.error } : {}),
      fetchedAt: Number(host.fetchedAt) || fetchedAt,
      ...(Array.isArray(host.spawnDirs) ? { spawnDirs: host.spawnDirs } : {}),
      ...(Array.isArray(host.hiddenSpawnDirs) ? { hiddenSpawnDirs: host.hiddenSpawnDirs } : {}),
      ...(typeof host.home === 'string' && host.home ? { home: host.home } : {}),
      ...(host.notes && typeof host.notes === 'object' ? { notes: host.notes } : {}),
      ...(Array.isArray(host.stacks) ? { stacks: host.stacks } : {}),
      sessions,
    };
  } catch (err) {
    return { name, ok: false, error: `unreachable: ${describeError(err, timeoutMs)}`, fetchedAt, sessions: [] };
  }
}

/**
 * Proxy a single-host request to a peer, forcing ?local=1 so it is never re-proxied.
 * Returns { status, body } where body is already-parsed JSON (or an error envelope).
 */
export async function proxyToPeer(baseUrl, { method = 'GET', pathname, search = '', body = null, timeoutMs = 15000 } = {}) {
  const url = `${baseUrl}${pathname}?${withLocalFlag(search)}`;
  try {
    const res = await fetchWithTimeout(
      url,
      {
        method,
        headers: {
          accept: 'application/json',
          ...(body != null ? { 'content-type': 'application/json' } : {}),
        },
        ...(body != null ? { body } : {}),
      },
      timeoutMs,
    );
    const text = await res.text();
    let parsed;
    try {
      parsed = text ? JSON.parse(text) : {};
    } catch {
      return { status: 502, body: { error: 'peer returned non-JSON response' } };
    }
    return { status: res.status, body: parsed };
  } catch (err) {
    const message = describeError(err, timeoutMs);
    const status = /timed out/.test(message) ? 504 : 502;
    return { status, body: { error: `peer ${baseUrl} unreachable: ${message}` } };
  }
}

/**
 * Proxy a request whose body is a stream (an upload) to a peer without buffering it:
 * `req` is piped into the peer request, `content-type` / `content-length` pass through.
 * The peer may answer before the body is done (413): the rest is then drained locally.
 * Returns { status, body } like proxyToPeer.
 */
export function streamToPeer(baseUrl, { method = 'POST', pathname, search = '', req, timeoutMs = 15 * 60 * 1000 } = {}) {
  const target = new URL(`${baseUrl}${pathname}?${withLocalFlag(search)}`);
  const headers = { accept: 'application/json' };
  for (const h of ['content-type', 'content-length']) if (req.headers?.[h]) headers[h] = req.headers[h];
  return new Promise((resolve) => {
    let settled = false;
    const done = (result) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      req.unpipe(out);
      if (!req.readableEnded) req.resume();
      resolve(result);
    };
    const out = (target.protocol === 'https:' ? https : http).request(target, { method, headers, agent: proxyAgentFor(target) }, (res) => {
      const chunks = [];
      res.on('data', (c) => chunks.push(c));
      res.on('error', (err) => done({ status: 502, body: { error: `peer ${baseUrl} unreachable: ${err.message}` } }));
      res.on('end', () => {
        const text = Buffer.concat(chunks).toString('utf8');
        try {
          done({ status: res.statusCode ?? 502, body: text ? JSON.parse(text) : {} });
        } catch {
          done({ status: 502, body: { error: 'peer returned non-JSON response' } });
        }
      });
    });
    const timer = setTimeout(() => {
      out.destroy();
      done({ status: 504, body: { error: `peer ${baseUrl} unreachable: timed out after ${timeoutMs}ms` } });
    }, timeoutMs);
    out.on('error', (err) => {
      // A peer that answered early (413) and closed shows up here as EPIPE/ECONNRESET after
      // the response; `settled` then wins.
      done({ status: 502, body: { error: `peer ${baseUrl} unreachable: ${describeError(err, timeoutMs)}` } });
    });
    req.on('close', () => {
      if (!req.complete) {
        out.destroy();
        done({ status: 400, body: { error: 'upload aborted by the client' } });
      }
    });
    req.pipe(out);
  });
}

const PASS_HEADERS = [
  'content-type',
  'content-length',
  'content-disposition',
  'content-security-policy',
  'x-content-type-options',
  'cache-control',
  'last-modified',
  'x-fleet-kind',
];

/**
 * GET a (possibly large, binary) response from a peer and hand its body back as a stream,
 * unbuffered: `{ status, headers, stream }` (the app pipes it to the client). The timeout
 * covers connecting and the response head only; once the body flows it is not cut off.
 * An unreachable peer → `{ status: 502|504, body }` like proxyToPeer.
 */
export function streamFromPeer(baseUrl, { pathname, search = '', timeoutMs = 20000 } = {}) {
  const target = new URL(`${baseUrl}${pathname}?${withLocalFlag(search)}`);
  return new Promise((resolve) => {
    let settled = false;
    const done = (result) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve(result);
    };
    const { url, options } = dialTarget(target);
    const out = (target.protocol === 'https:' ? https : http).request(url, { ...options, method: 'GET', agent: proxyAgentFor(target) }, (res) => {
      const headers = {};
      for (const h of PASS_HEADERS) if (res.headers[h] != null) headers[h] = res.headers[h];
      done({ status: res.statusCode ?? 502, headers, stream: res });
    });
    const timer = setTimeout(() => {
      out.destroy();
      done({ status: 504, body: { error: `peer ${baseUrl} unreachable: timed out after ${timeoutMs}ms` } });
    }, timeoutMs);
    out.on('error', (err) => done({ status: 502, body: { error: `peer ${baseUrl} unreachable: ${describeError(err, timeoutMs)}` } }));
    out.end();
  });
}
