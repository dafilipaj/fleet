import test from 'node:test';
import assert from 'node:assert/strict';
import { normalizeConfig } from '../lib/config.mjs';
import { createApi } from '../lib/api.mjs';
import { createHttpServer } from '../lib/app.mjs';

// A stand-in bridge: remembers what it was asked, holds its links in memory.
function fakeBridge(prefix, host, account) {
  const links = [];
  const calls = [];
  return {
    calls,
    links,
    status: async () => ({ enabled: true, host, ...account, links: links.filter((l) => !l.ended), ended: links.filter((l) => l.ended) }),
    link: async (o) => {
      calls.push(['link', o.host, o.session]);
      const l = { id: `${prefix}-${String(links.length + 1).padStart(6, '0')}`, host: o.host, session: o.session, url: o.url };
      links.push(l);
      return l;
    },
    unlink: async (id) => {
      calls.push(['unlink', id]);
      const l = links.find((x) => x.id === id);
      if (!l) throw Object.assign(new Error('no such link'), { status: 404 });
      l.ended = true;
      l.endedAt = 1;
      return l;
    },
    comment: async (o) => (calls.push(['comment', o.host, o.session]), { url: 'https://example.test/c' }),
  };
}

test('two bridges: github.bridges / productive.bridges send a host\'s sessions to its own bridge; status and unlink span both', async (t) => {
  const start = async (raw, bridges = {}) => {
    const config = normalizeConfig(raw, { env: {}, home: '/home/tester' });
    const fleet = { localHost: async () => ({ name: raw.self, ok: true, sessions: [] }) };
    const api = createApi({ config, fleet, backend: {}, transcripts: {}, spawner: {}, warmFleet: false, ...bridges });
    t.after(() => api.stop());
    const server = createHttpServer({ handleApi: api, uiDir: null });
    await new Promise((res) => server.listen(0, '127.0.0.1', res));
    t.after(() => server.close());
    return `http://127.0.0.1:${server.address().port}`;
  };
  const call = async (u, method = 'GET', body) => {
    const res = await fetch(u, { method, headers: body ? { 'content-type': 'application/json' } : {}, body: body ? JSON.stringify(body) : undefined });
    return { status: res.status, body: await res.json() };
  };

  const ghHub = fakeBridge('gl', 'hub', { login: 'bot-one' });
  const ghStudio = fakeBridge('gl', 'studio', { login: 'bot-two' });
  const prHub = fakeBridge('pl', 'hub', { me: { id: '1', name: 'Alex' } });
  const prStudio = fakeBridge('pl', 'studio', { me: { id: '2', name: 'Sam' } });
  const studioUrl = await start({ self: 'studio', github: { host: 'studio' }, productive: { host: 'studio', org: '1' } }, { github: ghStudio, productive: prStudio });
  const hubUrl = await start({ self: 'hub', hosts: { studio: { web: studioUrl }, laptop: { web: 'http://127.0.0.1:9' } }, github: { host: 'hub' }, productive: { host: 'hub', org: '1' } }, { github: ghHub, productive: prHub });
  const routes = { host: 'hub', bridges: { studio: 'studio' } };
  const laptop = await start({ self: 'laptop', hosts: { hub: { web: hubUrl }, studio: { web: studioUrl } }, github: routes, productive: routes });

  for (const [kind, hub, studio, account] of [
    ['github', ghHub, ghStudio, (b) => b.login],
    ['productive', prHub, prStudio, (b) => b.me?.name],
  ]) {
    // A studio session goes to the studio bridge; a laptop one to the main bridge, with its host stamped.
    const s = await call(`${laptop}/api/${kind}/link`, 'POST', { host: 'studio', session: 's1', url: 'https://example.test/1' });
    assert.equal(s.status, 200, JSON.stringify(s.body));
    const l = await call(`${laptop}/api/${kind}/link`, 'POST', { session: 's2', url: 'https://example.test/2' });
    assert.equal(l.status, 200, JSON.stringify(l.body));
    assert.deepEqual(studio.calls, [['link', 'studio', 's1']]);
    assert.deepEqual(hub.calls, [['link', 'laptop', 's2']]);

    // Status from anywhere: both bridges' links, each with its bridge; every bridge's account.
    const st = await call(`${laptop}/api/${kind}`);
    assert.equal(st.body.host, 'hub');
    assert.deepEqual(st.body.links.map((x) => [x.host, x.session, x.bridge]).sort(), [['laptop', 's2', 'hub'], ['studio', 's1', 'studio']]);
    const names = kind === 'github' ? ['bot-one', 'bot-two'] : ['Alex', 'Sam'];
    assert.deepEqual(st.body.bridges.map((b) => [b.host, b.enabled, account(b)]), [['hub', true, names[0]], ['studio', true, names[1]]]);
    // A bridge's own view stays its own.
    assert.equal((await call(`${studioUrl}/api/${kind}`)).body.links.length, 1);

    // Unlink on the bridge named; an unknown bridge is refused.
    assert.equal((await call(`${laptop}/api/${kind}/links/${s.body.id}/unlink`, 'POST', { bridge: 'studio' })).status, 200);
    assert.deepEqual(studio.calls.at(-1), ['unlink', s.body.id]);
    assert.equal(hub.calls.filter((c) => c[0] === 'unlink').length, 0);
    assert.equal((await call(`${laptop}/api/${kind}/links/${l.body.id}/unlink`, 'POST', { bridge: 'elsewhere' })).status, 404);
    assert.equal((await call(`${laptop}/api/${kind}/links/${l.body.id}/unlink`, 'POST', {})).status, 200);
    assert.deepEqual(hub.calls.at(-1), ['unlink', l.body.id]);
  }

  // A studio session's comment, sent through the laptop, reaches the studio bridge.
  assert.equal((await call(`${laptop}/api/productive/comment`, 'POST', { host: 'studio', session: 's1', body: 'hi' })).status, 200);
  assert.deepEqual(prStudio.calls.at(-1), ['comment', 'studio', 's1']);
});

test('config: github.bridges / productive.bridges must map hosts to host names', () => {
  const c = normalizeConfig({ self: 'a', github: { host: 'hub', bridges: { studio: ' studio ' } }, productive: { host: 'hub' } }, { env: {}, home: '/home/tester' });
  assert.deepEqual(c.github.bridges, { studio: 'studio' });
  assert.deepEqual(c.productive.bridges, {});
  assert.throws(() => normalizeConfig({ self: 'a', github: { bridges: ['x'] } }, { env: {}, home: '/h' }), /github\.bridges must be an object/);
  assert.throws(() => normalizeConfig({ self: 'a', productive: { bridges: { x: '' } } }, { env: {}, home: '/h' }), /productive\.bridges\.x must be a host name/);
});
