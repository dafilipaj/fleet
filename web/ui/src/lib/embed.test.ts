import { expect, test } from 'vitest'

import type { FleetResponse, Session } from '@/api/types'
import { embedHash, isInside, parseEmbedHash, readEmbed, resolveEmbedDir, sessionsIn } from './embed'

test('parseEmbedHash: host + dir required, theme optional', () => {
  expect(parseEmbedHash('#/embed?host=workstation&dir=~/Code/project&theme=light')).toEqual({ host: 'workstation', dir: '~/Code/project', theme: 'light', app: 'project' })
  expect(parseEmbedHash('#/embed?host=w&dir=%2Fsrv%2Fapp')).toEqual({ host: 'w', dir: '/srv/app', theme: null, app: 'app' })
  expect(parseEmbedHash('#/embed?host=w&dir=/srv/x&app=notes')?.app).toBe('notes')
  expect(parseEmbedHash('#/embed?host=w&dir=/srv/x&app=a%20b')?.app).toBe('x')
  expect(parseEmbedHash('#/embed?host=w&dir=/')?.app).toBe('app')
  expect(parseEmbedHash('#/embed?host=w&dir=/x&theme=blue')?.theme).toBeNull()
  expect(parseEmbedHash('#/embed?host=w')).toBeNull()
  expect(parseEmbedHash('#/embed')).toBeNull()
  expect(parseEmbedHash('#/s/w/abc')).toBeNull()
  expect(parseEmbedHash('#/embedded?host=w&dir=/x')).toBeNull()
})

test('embedHash round-trips', () => {
  const p = { host: 'w', dir: '~/Code/a b', theme: 'dark' as const, app: 'notes' }
  expect(parseEmbedHash(`#${embedHash(p)}`)).toEqual(p)
})

test('readEmbed: the hash, else the query string the router moved it to', () => {
  const p = { host: 'w', dir: '~/Code/app', theme: null, app: 'app' }
  expect(readEmbed({ hash: '#/embed?host=w&dir=~/Code/app', search: '' })).toEqual(p)
  expect(readEmbed({ hash: '#/embed', search: '?host=w&dir=~/Code/app' })).toEqual(p)
  expect(readEmbed({ hash: '#/s/w/abc', search: '?host=w&dir=~/Code/app' })).toEqual(p)
  expect(readEmbed({ hash: '#/s/w/abc', search: '' })).toBeNull()
})

test('isInside: the dir itself or beneath it, not a sibling with the same prefix', () => {
  expect(isInside('/h/Code/app', '/h/Code/app')).toBe(true)
  expect(isInside('/h/Code/app/.worktrees/x', '/h/Code/app/')).toBe(true)
  expect(isInside('/h/Code/app-2', '/h/Code/app')).toBe(false)
  expect(isInside(null, '/h')).toBe(false)
  expect(isInside('/anything', '/')).toBe(true)
})

test('resolveEmbedDir: only a spawn dir or beneath one', () => {
  const host = { home: '/home/me', spawnDirs: [{ label: 'Code', path: '/home/me/Code' }] }
  expect(resolveEmbedDir('~/Code/app', host)).toEqual({ ok: true, path: '/home/me/Code/app' })
  expect(resolveEmbedDir('/home/me/Code/', host)).toEqual({ ok: true, path: '/home/me/Code' })
  expect(resolveEmbedDir('~/Documents', host).ok).toBe(false)
  expect(resolveEmbedDir('/home/me/Code/../.ssh', host).ok).toBe(false)
  expect(resolveEmbedDir('Code/app', host).ok).toBe(false)
  expect(resolveEmbedDir('~/Code/app', { spawnDirs: [] }).ok).toBe(false)
  expect(resolveEmbedDir('~/Code/app', null).ok).toBe(false)
})

test('resolveEmbedDir: a hidden spawn dir (not in the menu) is allowed too', () => {
  const host = { home: '/home/me', spawnDirs: [{ label: 'Work', path: '/home/me/Code' }], hiddenSpawnDirs: [{ label: 'Brain', path: '/home/me/brain' }] }
  expect(resolveEmbedDir('~/brain', host)).toEqual({ ok: true, path: '/home/me/brain' })
  expect(resolveEmbedDir('~/brain/sub', host)).toEqual({ ok: true, path: '/home/me/brain/sub' })
  expect(resolveEmbedDir('~/other', host).ok).toBe(false)
})

test('resolveEmbedDir without home (an older server): a spawn dir with the same tail', () => {
  const host = { spawnDirs: [{ label: 'App', path: '/home/me/Code/app' }] }
  expect(resolveEmbedDir('~/Code/app', host)).toEqual({ ok: true, path: '/home/me/Code/app' })
  expect(resolveEmbedDir('~/Code/other', host).ok).toBe(false)
})

test('sessionsIn: that host, that dir (worktrees included), latest first', () => {
  const s = (over: Partial<Session>): Session => ({ host: 'w', session_id: 'x', status: 'idle', ...over })
  const fleet: FleetResponse = {
    self: 'w',
    hosts: [
      {
        name: 'w',
        ok: true,
        sessions: [
          s({ session_id: 'a', cwd: '/h/app', updated_at: 1 }),
          s({ session_id: 'b', cwd: '/h/app/.worktrees/t', updated_at: 3 }),
          s({ session_id: 'c', cwd: '/h/app-old', updated_at: 5 }),
          s({ session_id: 'd', cwd: '/h/app', updated_at: 9, operator: true }),
        ],
      },
      { name: 'v', ok: true, sessions: [s({ host: 'v', session_id: 'e', cwd: '/h/app', updated_at: 7 })] },
    ],
  }
  expect(sessionsIn(fleet, 'w', '/h/app').map((x) => x.session_id)).toEqual(['b', 'a'])
})
