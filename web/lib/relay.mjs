// web.relay: a filtered HTTP CONNECT proxy, for a peer that cannot reach a host this one can.
//
// It answers `CONNECT host:port` only, and only for a target in `allow` (an exact, lowercase
// `host:port` match: no wildcards, no normalisation, no fallthrough). An allowed target is dialled
// through the upstream HTTP proxy `via` (its own CONNECT, which must answer 200) or directly; the
// client then gets `200 Connection Established` and the bytes flow both ways, unread. Anything
// else: a target not allowed → 403, any other method → 405, the upstream failing → 502 (504 on a
// timeout). Whoever reaches `listen` gets those targets and nothing more: keep it on a tailnet IP.
import http from 'node:http';
import net from 'node:net';

const MAX_HEAD = 16 * 1024;
const TARGET_RE = /^([A-Za-z0-9.-]+):(\d{1,5})$/;

function reply(socket, status, text) {
  if (socket.destroyed) return;
  socket.end(`HTTP/1.1 ${status} ${text}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n`);
}

/**
 * Open a raw socket to `target` (`host:port`), through `via` ({ host, port }) when given.
 * Resolves with { socket, head } (`head`: bytes the upstream sent after its 200), rejects with an
 * Error whose `status` is the code to give the client.
 */
export function dialTunnel(target, { via = null, timeoutMs = 10000 } = {}) {
  const [, host, port] = TARGET_RE.exec(target);
  return new Promise((resolve, reject) => {
    const socket = via ? net.connect(via.port, via.host) : net.connect(Number(port), host);
    let settled = false;
    const fail = (message, status = 502) => {
      if (settled) return;
      settled = true;
      socket.destroy();
      reject(Object.assign(new Error(message), { status }));
    };
    const ok = (head) => {
      if (settled) return;
      settled = true;
      socket.setTimeout(0);
      socket.removeAllListeners('data');
      socket.removeAllListeners('error');
      socket.removeAllListeners('end');
      resolve({ socket, head });
    };
    socket.setTimeout(timeoutMs, () => fail(`timed out after ${timeoutMs}ms`, 504));
    socket.on('error', (err) => fail(err.message));
    socket.on('end', () => fail('upstream closed the connection'));
    socket.on('connect', () => {
      if (!via) return ok(Buffer.alloc(0));
      socket.write(`CONNECT ${target} HTTP/1.1\r\nHost: ${target}\r\n\r\n`);
      let buf = Buffer.alloc(0);
      socket.on('data', (chunk) => {
        buf = Buffer.concat([buf, chunk]);
        const end = buf.indexOf('\r\n\r\n');
        if (end < 0) {
          if (buf.length > MAX_HEAD) fail('upstream proxy sent an oversized response head');
          return;
        }
        const statusLine = buf.subarray(0, buf.indexOf('\r\n')).toString('latin1');
        const m = /^HTTP\/1\.[01] (\d{3})/.exec(statusLine);
        if (!m || m[1] !== '200') return fail(`upstream proxy refused: ${statusLine.slice(0, 100)}`);
        ok(buf.subarray(end + 4));
      });
    });
  });
}

/**
 * The relay server (not listening yet). `allow`: exact `host:port` targets; `via`: { host, port }
 * of the upstream HTTP proxy, or null to dial directly.
 */
export function createRelay({ allow, via = null, timeoutMs = 10000, log = () => {} }) {
  const allowed = new Set(allow.map((t) => t.toLowerCase()));
  const open = new Set();
  const track = (s) => {
    open.add(s);
    s.on('close', () => open.delete(s));
  };

  const server = http.createServer((req, res) => {
    log(`[relay] refused ${String(req.method).slice(0, 16)} from ${req.socket.remoteAddress}: only CONNECT`);
    res.writeHead(405, { allow: 'CONNECT', connection: 'close', 'content-length': 0 });
    res.end();
  });

  server.on('connect', (req, client, head) => {
    track(client);
    client.on('error', () => client.destroy());
    const target = String(req.url ?? '').toLowerCase();
    if (!TARGET_RE.test(target) || !allowed.has(target)) {
      log(`[relay] refused CONNECT ${target.slice(0, 200)} from ${client.remoteAddress}: not allowed`);
      return reply(client, 403, 'Forbidden');
    }
    dialTunnel(target, { via, timeoutMs }).then(
      ({ socket: upstream, head: early }) => {
        track(upstream);
        if (client.destroyed) return upstream.destroy();
        const closeBoth = () => {
          client.destroy();
          upstream.destroy();
        };
        upstream.on('error', closeBoth);
        client.on('error', closeBoth);
        upstream.on('close', closeBoth);
        client.on('close', closeBoth);
        client.write('HTTP/1.1 200 Connection Established\r\n\r\n');
        if (early.length) client.write(early);
        if (head?.length) upstream.write(head);
        upstream.pipe(client);
        client.pipe(upstream);
      },
      (err) => {
        log(`[relay] CONNECT ${target} failed: ${err.message}`);
        reply(client, err.status ?? 502, err.status === 504 ? 'Gateway Timeout' : 'Bad Gateway');
      },
    );
  });

  server.on('clientError', (_err, socket) => reply(socket, 400, 'Bad Request'));

  // close() also ends the tunnels in flight (http.Server.close leaves CONNECT sockets open).
  const close = server.close.bind(server);
  server.close = (cb) => {
    close(cb);
    for (const s of open) s.destroy();
    return server;
  };
  return server;
}
