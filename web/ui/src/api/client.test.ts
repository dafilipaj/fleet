import { afterEach, describe, expect, it, vi } from 'vitest'

import { ApiError, request } from './client'
import { setLoadConfirm } from '@/lib/loadConfirm'

const reply = (status: number, body: unknown) => new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } })
const overloaded = () => reply(409, { error: 'workstation is under heavy load (load 3× cores) — new sessions wait until it recovers', overloaded: true })

afterEach(() => {
  setLoadConfirm(null)
  vi.unstubAllGlobals()
})

describe('request on a host under heavy load', () => {
  it('asks, then sends the same request with ignoreLoad', async () => {
    const fetch = vi.fn().mockResolvedValueOnce(overloaded()).mockResolvedValueOnce(reply(200, { ok: true }))
    vi.stubGlobal('fetch', fetch)
    const asked: string[] = []
    setLoadConfirm(async (m) => (asked.push(m), true))
    await expect(request('/api/hosts/workstation/spawn', { method: 'POST', body: { name: 'x', dir: '/d' } })).resolves.toEqual({ ok: true })
    expect(asked[0]).toMatch(/under heavy load/)
    expect(JSON.parse(fetch.mock.calls[1][1].body)).toEqual({ name: 'x', dir: '/d', ignoreLoad: true })
  })

  it('fails with the server message when you say no, or when nothing can ask', async () => {
    vi.stubGlobal('fetch', vi.fn().mockImplementation(async () => overloaded()))
    setLoadConfirm(async () => false)
    await expect(request('/api/x', { method: 'POST', body: {} })).rejects.toBeInstanceOf(ApiError)
    setLoadConfirm(null)
    await expect(request('/api/x', { method: 'POST', body: {} })).rejects.toThrow(/under heavy load/)
  })

  it('leaves any other 409 alone', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(reply(409, { error: 'a session of the job works', busy: true })))
    const ask = vi.fn(async () => true)
    setLoadConfirm(ask)
    await expect(request('/api/x', { method: 'POST', body: {} })).rejects.toThrow(/session of the job works/)
    expect(ask).not.toHaveBeenCalled()
  })
})
