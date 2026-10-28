// Demo API for the fleet web UI, for the feature-guide screenshots (scripts/guide/README.md).
// Every value here is invented: company "Acme", user "demo", hosts laptop / workstation / devbox.
// It serves web/ui/dist and answers the HTTP API the UI calls (web/ui/src/api/client.ts) with
// canned data; writes answer { ok: true } and change nothing. New UI features need data here.
import http from 'node:http'
import fs from 'node:fs'
import path from 'node:path'

const DIST = process.env.FLEET_WEB_UI || new URL('../../web/ui/dist', import.meta.url).pathname
const PORT = Number(process.env.GUIDE_PORT || 7799)
const H = '/Users/demo'
const M = 60_000, HR = 3_600_000, D = 86_400_000
const now = () => Date.now()
const iso = (ms) => new Date(ms).toISOString()

// ---------------------------------------------------------------- sessions
const S = (o) => ({ backend: 'tmux', harness: 'claude', name_source: 'generated', stack: null, origin: null, tty: '/dev/ttys004', ...o })
const SESSIONS = () => [
  S({ host: 'workstation', session_id: 'a1b2c3d4-1111-4000-8000-000000000001', pid: 41201, display_title: 'Invoice rounding fix', gen_title: 'Invoice rounding fix', cwd: `${H}/Code/acme-api/.worktrees/fix-invoice-rounding`, status: 'busy', updated_at: now() - 4_000, handle: '%12', tmux_session: 'invoice-rounding-fix', model: 'claude-opus-5-5', context: { used: 92_000, window: 200_000, pct: 46, model: 'claude-opus-5-5' }, title: 'Invoices with 3+ line items are off by a cent vs. the PDF…' }),
  S({ host: 'workstation', session_id: 'a1b2c3d4-1111-4000-8000-000000000002', pid: 41388, display_title: 'Checkout e2e flake', cwd: `${H}/Code/acme-web`, status: 'waiting', waiting_for: 'permission: Bash(npm run test:e2e -- checkout)', updated_at: now() - 95_000, handle: '%15', tmux_session: 'checkout-e2e-flake', model: 'claude-sonnet-5-5', context: { used: 122_000, window: 200_000, pct: 61 } }),
  S({ host: 'workstation', session_id: 'a1b2c3d4-1111-4000-8000-000000000003', pid: 41420, display_title: 'Rate limits spec', cwd: `${H}/Code/acme-api`, status: 'waiting', waiting_for: 'question: Which limit model should v1 use?', updated_at: now() - 40_000, handle: '%17', tmux_session: 'rate-limits-spec', model: 'claude-opus-5-5', context: { used: 38_000, window: 200_000, pct: 19 } }),
  S({ host: 'workstation', session_id: 'a1b2c3d4-1111-4000-8000-000000000004', pid: 41502, display_title: 'Dependency audit', cwd: `${H}/Code/acme-web`, status: 'idle', updated_at: now() - 26 * M, handle: '%21', tmux_session: 'job-dependency-audit', origin: { job: 'dependency-audit', run: '20261015-0700' }, model: 'claude-sonnet-5-5', context: { used: 54_000, window: 200_000, pct: 27 } }),
  S({ host: 'workstation', session_id: 'a1b2c3d4-1111-4000-8000-000000000005', pid: 41630, display_title: 'Search index migration', harness: 'codex', cwd: `${H}/Code/search-indexer`, status: 'busy', updated_at: now() - 9_000, handle: '%24', tmux_session: 'search-index-migration', model: 'gpt-5.5-codex', context: null }),
  S({ host: 'workstation', session_id: 'a1b2c3d4-1111-4000-8000-000000000006', pid: 41702, display_title: 'Review acme-web#1310', cwd: `${H}/Code/acme-web/.worktrees/pr-1310`, status: 'idle', updated_at: now() - 12 * M, handle: '%26', tmux_session: 'review-acme-web-1310', done: { at: now() - 12 * M, note: 'Reviewed acme-web#1310: 2 blocking comments (retry loop, missing index), approved the rest' }, model: 'claude-opus-5-5', context: { used: 71_000, window: 200_000, pct: 36 } }),
  S({ host: 'devbox', session_id: 'b2c3d4e5-2222-4000-8000-000000000001', pid: 9120, display_title: 'Onboarding flow redesign', cwd: `${H}/Code/acme-web`, status: 'busy', updated_at: now() - 2_000, handle: '%3', tmux_session: 'onboarding-flow', model: 'claude-opus-5-5', stack: { id: 'onboarding-redesign', label: 'Onboarding redesign', host: 'devbox' }, context: { used: 148_000, window: 200_000, pct: 74 } }),
  S({ host: 'devbox', session_id: 'b2c3d4e5-2222-4000-8000-000000000002', pid: 9188, display_title: 'Onboarding copy & emails', cwd: `${H}/Code/acme-web`, status: 'idle', updated_at: now() - 7 * M, handle: '%5', tmux_session: 'onboarding-copy', model: 'claude-sonnet-5-5', stack: { id: 'onboarding-redesign', label: 'Onboarding redesign', host: 'devbox' }, context: { used: 30_000, window: 200_000, pct: 15 } }),
  S({ host: 'workstation', session_id: 'a1b2c3d4-1111-4000-8000-000000000007', pid: 41790, display_title: 'Onboarding API endpoints', cwd: `${H}/Code/acme-api`, status: 'busy', updated_at: now() - 15_000, handle: '%28', tmux_session: 'onboarding-api', model: 'claude-sonnet-5-5', stack: { id: 'onboarding-redesign', label: 'Onboarding redesign', host: 'devbox' }, context: { used: 66_000, window: 200_000, pct: 33 } }),
  S({ host: 'devbox', session_id: 'b2c3d4e5-2222-4000-8000-000000000003', pid: 9301, display_title: 'Dashboard perf profiling', harness: 'pi', cwd: `${H}/Code/acme-web`, status: 'idle', updated_at: now() - 48 * M, handle: '%7', tmux_session: 'dashboard-perf', model: 'claude-sonnet-5-5', context: null }),
  S({ host: 'devbox', session_id: 'b2c3d4e5-2222-4000-8000-000000000004', pid: 9355, display_title: 'October release notes', cwd: `${H}/Code/docs`, status: 'waiting', waiting_for: 'Ready for your review', updated_at: now() - 3 * M, handle: '%9', tmux_session: 'release-notes-oct', model: 'claude-sonnet-5-5', context: { used: 44_000, window: 200_000, pct: 22 } }),
  S({ host: 'laptop', session_id: 'c3d4e5f6-3333-4000-8000-000000000001', pid: 77120, display_title: 'Docs site search', backend: 'iterm', handle: 'w0t2p0', tab: 'docs', cwd: `${H}/Code/docs`, status: 'idle', updated_at: now() - 2 * HR, model: 'claude-sonnet-5-5', context: { used: 18_000, window: 200_000, pct: 9 } }),
  S({ host: 'laptop', session_id: 'c3d4e5f6-3333-4000-8000-000000000002', pid: 77333, display_title: 'Billing webhook retries', cwd: `${H}/Code/billing-service`, status: 'busy', updated_at: now() - 6_000, handle: '%2', tmux_session: 'billing-webhooks', model: 'claude-opus-5-5', context: { used: 410_000, window: 1_000_000, pct: 41 } }),
  S({ host: 'workstation', session_id: 'a1b2c3d4-1111-4000-8000-0000000000ff', pid: 41999, display_title: 'Operator', cwd: `${H}/.local/state/fleet/operator`, status: 'idle', updated_at: now() - 60_000, handle: '%30', tmux_session: 'operator', operator: true, model: 'claude-sonnet-5-5' }),
].map((s) => ({ ...s, name: s.display_title, editorUrl: `vscode://vscode-remote/ssh-remote+${s.host}${s.cwd}` }))

const SPAWN_DIRS = {
  workstation: [{ label: 'acme-api', path: '~/Code/acme-api' }, { label: 'acme-web', path: '~/Code/acme-web' }, { label: 'billing-service', path: '~/Code/billing-service' }, { label: 'docs', path: '~/Code/docs' }, { label: 'scratch', path: '~/scratch' }],
  devbox: [{ label: 'acme-api', path: '~/Code/acme-api' }, { label: 'acme-web', path: '~/Code/acme-web' }, { label: 'docs', path: '~/Code/docs' }],
  laptop: [{ label: 'acme-web', path: '~/Code/acme-web' }, { label: 'docs', path: '~/Code/docs' }, { label: 'billing-service', path: '~/Code/billing-service' }],
}
const HOSTS = ['laptop', 'workstation', 'devbox']
function fleet() {
  const all = SESSIONS()
  return {
    self: 'laptop', snapshotAt: now(),
    hosts: HOSTS.map((h) => ({ name: h, ok: true, fetchedAt: now() - 1500, spawnDirs: SPAWN_DIRS[h], ...(h === 'devbox' ? { notes: { name: 'Team notes' } } : {}), sessions: all.filter((s) => s.host === h) })),
  }
}
const find = (host, id) => SESSIONS().find((s) => s.host === host && s.session_id === id)

// ---------------------------------------------------------------- messages
const PASTE = `<pasted_content id="p1">
ERROR invoice_mismatch invoice=INV-2026-0912 pdf_total=1249.99 api_total=1250.00 lines=3
ERROR invoice_mismatch invoice=INV-2026-0927 pdf_total=88.41 api_total=88.40 lines=4
</pasted_content id="p1">`
function chat(id) {
  const t = now()
  if (id.endsWith('0001') && id.startsWith('a1')) return [
    { role: 'user', kind: 'user', ts: t - 38 * M, text: `Invoices with 3+ line items are off by a cent vs. the PDF. Find where rounding happens and fix it so the totals match.\n${PASTE}` },
    { role: 'assistant', kind: 'assistant', final: false, ts: t - 37 * M, text: 'Looking at how line totals and the invoice total are computed.' },
    { role: 'assistant', kind: 'assistant', final: false, ts: t - 35 * M, text: 'The API rounds every line before summing, the PDF renderer sums first and rounds once.' },
    { role: 'assistant', kind: 'assistant', final: true, ts: t - 33 * M, text: `## Root cause\n\n\`src/billing/invoice_total.ts\` rounds **each line** to 2 decimals and then sums them. The PDF (\`src/pdf/totals.ts\`) sums the exact amounts and rounds once, so with 3+ lines the two can drift by a cent.\n\n| invoice | lines | API | PDF |\n|---|---|---|---|\n| INV-2026-0912 | 3 | 1250.00 | 1249.99 |\n| INV-2026-0927 | 4 | 88.40 | 88.41 |\n\n**Fix:** keep amounts in integer cents, sum exactly, round once at the end (banker's rounding, same as the PDF). I added a regression test in \`test/billing/invoice_total.test.ts\` with both invoices.\n\nThe plan is written up in \`docs/billing/rounding.md\`. Should I open the PR?` },
    { role: 'user', kind: 'user', ts: t - 20 * M, text: 'Yes — open the PR, link issue #1777 and ask Marta for review.' },
    { role: 'user', kind: 'command', name: '!', args: 'npm test -- billing', ts: t - 19 * M, text: '! npm test -- billing' },
    { role: 'assistant', kind: 'assistant', final: false, ts: t - 18 * M, text: 'All 214 billing tests pass. Pushing the branch and opening the PR.' },
    { role: 'assistant', kind: 'assistant', final: true, ts: t - 16 * M, text: `Opened **[acme/acme-api#482](https://github.com/acme/acme-api/pull/482)** — *Fix invoice total rounding (sum in cents, round once)*.\n\n- Closes #1777\n- Review requested from @marta\n- CI is running; I'll watch it and fix anything that fails.` },
    { role: 'user', kind: 'command', name: '/code-review', args: 'focus on currency edge cases (JPY, 3-decimal currencies)', ts: t - 2 * M, text: '/code-review focus on currency edge cases' },
    { role: 'assistant', kind: 'assistant', final: false, ts: t - 60_000, text: 'Checking how `Currency.minorUnits` is used across billing…' },
  ]
  if (id.endsWith('0006') && id.startsWith('a1')) return [
    { role: 'user', kind: 'user', ts: t - 25 * M, text: 'Review acme-web#1310 and leave the comments on GitHub.' },
    { role: 'assistant', kind: 'assistant', final: false, ts: t - 20 * M, text: 'Reading the diff: 14 files, the sync worker and a migration.' },
    { role: 'assistant', kind: 'assistant', final: true, ts: t - 12 * M, text: `Reviewed **[acme/acme-web#1310](https://github.com/acme/acme-web/pull/1310)** and left the comments on GitHub.\n\n- **Blocking:** the retry loop in \`sync/worker.ts\` never backs off\n- **Blocking:** the migration adds \`orders.customer_id\` without an index\n- Approved the rest; two nits on naming\n\nWorth checking: whether you want the index in this PR or a follow-up.` },
  ]
  if (id.endsWith('0003') && id.startsWith('a1')) return [
    { role: 'user', kind: 'user', ts: t - 12 * M, text: 'Write a spec for per-organization API rate limits. Look at how the gateway counts requests today first.' },
    { role: 'assistant', kind: 'assistant', final: false, ts: t - 9 * M, text: 'The gateway counts per token in Redis with a fixed 60 s window (`gateway/limits.go`).' },
    { role: 'assistant', kind: 'assistant', final: true, ts: t - 2 * M, text: 'I have the current picture. Before I write the spec I need one decision from you.' },
    { role: 'assistant', kind: 'question', id: 'toolu_demo_q1', ts: t - 40_000, final: true, text: 'Which limit model should v1 use?', questions: [{ question: 'Which limit model should v1 use?', header: 'Limit model', multiSelect: false, options: [
      { label: 'Token bucket per org (Recommended)', description: 'Smooth bursts, one Redis key per org, easy to explain to customers.' },
      { label: 'Fixed window per token', description: 'What the gateway does today; cheapest, but bursty at window edges.' },
      { label: 'Sliding window log', description: 'Most precise, but one sorted set per org and more Redis memory.' },
    ] }] },
  ]
  if (id.endsWith('0002') && id.startsWith('a1')) return [
    { role: 'user', kind: 'user', ts: t - 30 * M, text: 'checkout.spec.ts fails ~1 in 5 runs on CI. Find out why and make it stable.' },
    { role: 'assistant', kind: 'assistant', final: true, ts: t - 4 * M, text: 'The flake is a race: the test clicks **Pay** before the Stripe iframe reports ready. I changed it to wait for `data-ready="true"`. I want to run the suite 20× to confirm.' },
  ]
  if (id === 'a1b2c3d4-1111-4000-8000-0000000000ff') return [
    { role: 'user', kind: 'user', ts: t - 3 * M, text: "What's happening on workstation right now?" },
    { role: 'assistant', kind: 'assistant', final: true, ts: t - 3 * M + 9000, text: 'Six sessions on workstation. Two need you: Checkout e2e flake wants permission to run the e2e suite, and Rate limits spec has a question about the limit model. Invoice rounding and Search index migration are working.' },
    { role: 'user', kind: 'user', ts: t - 2 * M, text: 'Allow the checkout one to run the tests.' },
    { role: 'assistant', kind: 'assistant', final: true, ts: t - 2 * M + 11000, text: 'Done — I approved it. Checkout e2e flake is running the suite now.' },
  ]
  return [
    { role: 'user', kind: 'user', ts: t - 50 * M, text: 'Continue where we left off — the onboarding step 2 layout and the API it needs.' },
    { role: 'assistant', kind: 'assistant', final: true, ts: t - 5 * M, text: 'Step 2 now uses the new `OnboardingStep` component. Next: wire it to `POST /v1/onboarding/profile` once the API session lands it.' },
  ]
}

const PEEK = `  ⎿  Read src/billing/invoice_total.ts (184 lines)
  ⎿  Read src/billing/currency.ts (96 lines)

⏺ Search(pattern: "minorUnits", path: "src")
  ⎿  Found 11 files

⏺ Bash(npm test -- billing --reporter=dot)
  ⎿  ················································
     ················································
     ··············································
     214 passing (3.2s)

⏺ The rounding fix holds for 2-decimal currencies. Checking JPY (0
  decimals) and KWD (3 decimals) next.

⏺ Update(src/billing/invoice_total.ts)
  ⎿  Updated src/billing/invoice_total.ts with 6 additions and 2 removals
       38    export function invoiceTotal(lines: Line[], cur: Currency) {
       39 -    return lines.reduce((s, l) => s + round(l.amount, 2), 0)
       39 +    const cents = lines.reduce((s, l) => s + toMinor(l.amount, cur), 0n)
       40 +    return fromMinor(cents, cur)
       41    }

✻ Reviewing currency edge cases… (2m 14s · ↓ 3.1k tokens · esc to interrupt)

╭──────────────────────────────────────────────────────────────────────────────╮
│ >                                                                            │
╰──────────────────────────────────────────────────────────────────────────────╯
  ⏵⏵ accept edits on (shift+tab to cycle)                    Context: 46% used`

// ---------------------------------------------------------------- briefs
function brief(host, id) {
  const s = find(host, id)
  const big = id.endsWith('0001') && id.startsWith('a1')
  const summary = big
    ? 'Fixing invoice total rounding in acme-api on branch `fix-invoice-rounding` (workstation): totals are now summed in integer cents and rounded once. PR #482 is open and in review; checking 0- and 3-decimal currencies next.'
    : `Working on ${s?.display_title ?? 'this task'} in ${s?.cwd?.replace(H, '~')}.`
  const todos = big
    ? [{ done: true, text: 'Find where line totals are rounded' }, { done: true, text: 'Sum in cents, round once' }, { done: true, text: 'Regression test with INV-2026-0912 and -0927' }, { done: false, text: 'Check JPY / KWD (0 and 3 decimals)' }, { done: false, text: 'Address review comments on #482' }]
    : [{ done: true, text: 'Read the current implementation' }, { done: false, text: 'Finish and hand over' }]
  const resources = big ? [
    { kind: 'Origin', label: 'Invoice totals off by a cent', url: 'https://app.acme.dev/tasks/2041', path: null, text: 'Origin' },
    { kind: 'Origin', label: 'acme.slack.com/archives/C04BILLING/p1760000000', url: 'https://acme.slack.com/archives/C04BILLING/p1760000000', path: null, text: 'Origin' },
    { kind: 'Git', label: null, url: null, path: '~/Code/acme-api/.worktrees/fix-invoice-rounding', text: 'Git: `fix-invoice-rounding` · worktree', branch: 'fix-invoice-rounding', linked: true },
    { kind: 'PR', label: 'acme/acme-api#482', url: 'https://github.com/acme/acme-api/pull/482', path: null, text: 'PR' },
    { kind: 'Issue', label: 'acme/acme-api#1777', url: 'https://github.com/acme/acme-api/issues/1777', path: null, text: 'Issue' },
    { kind: 'Spec', label: null, url: null, path: 'docs/billing/rounding.md', text: 'Spec' },
    { kind: 'File', label: null, url: null, path: 'src/billing/invoice_total.ts', text: 'File' },
    { kind: 'File', label: null, url: null, path: 'src/pdf/totals.ts', text: 'File' },
    { kind: 'File', label: null, url: null, path: 'test/billing/invoice_total.test.ts', text: 'File' },
    { kind: 'Link', label: 'Rounding in accounting (wiki)', url: 'https://en.wikipedia.org/wiki/Rounding', path: null, text: 'Link' },
  ] : []
  const md = `---\nsession: ${id}\nhost: ${host}\n---\n## Summary\n${summary}\n\n## Todos\n${todos.map((t) => `- [${t.done ? 'x' : ' '}] ${t.text}`).join('\n')}\n`
  return {
    host, id, exists: true, markdown: md, parsed: { summary, resources, todos, plan: todos },
    updated: iso(now() - 3 * M), editedAt: null, generatedAt: iso(now() - 3 * M), generatedThrough: 1000, generating: false, enabled: true,
    continuePrompt: `Continue the work of session ${id} on ${host}. Its brief:\n\n${summary}\n\n---`,
    absCwd: s?.cwd ?? null, gitRoot: s?.cwd ?? null, editor: 'vscode', editorUrl: s?.editorUrl ?? null,
    worktrees: s?.cwd ? [{ path: s.cwd, display: s.cwd.replace(H, '~'), branch: big ? 'fix-invoice-rounding' : 'main', linked: big, editorUrl: s.editorUrl }] : [],
  }
}

// ---------------------------------------------------------------- groups
function groups() {
  const m = (host, tail) => ({ host, id: SESSIONS().find((s) => s.host === host && s.session_id.endsWith(tail)).session_id })
  return {
    enabled: true, host: 'workstation', intervalMinutes: 10, running: false, updatedAt: now() - 3 * M,
    lastRun: { at: now() - 3 * M, ms: 14_200, ok: true, reason: 'scheduled', mode: 'incremental', modelCalls: 1, classified: 2 },
    groups: [
      { id: 'g-billing', label: 'Billing & invoices', description: 'Rounding, webhooks and payment retries', source: 'llm', members: [m('workstation', '0001'), m('laptop', '0002')] },
      { id: 'g-checkout', label: 'Checkout reliability', description: 'Flaky e2e tests and checkout review', source: 'llm', members: [m('workstation', '0002'), m('workstation', '0006')] },
      { id: 'g-platform', label: 'API platform', description: 'Rate limits and search infrastructure', source: 'llm', members: [m('workstation', '0003'), m('workstation', '0005'), { host: 'workstation', id: 'dormant-legacy-import', dormant: true }] },
      { id: 'g-docs', label: 'Docs & releases', description: 'Release notes and docs site', source: 'llm', members: [m('devbox', '0004'), m('laptop', '0001')] },
      { id: 'g-maint', label: 'Maintenance', description: 'Audits and performance', source: 'llm', members: [m('workstation', '0004'), m('devbox', '0003')] },
    ],
  }
}

// ---------------------------------------------------------------- stacks
function stack() {
  const md = `---\nlabel: Onboarding redesign\n---\n## Summary\nNew 3-step onboarding for Acme web: UI in acme-web (devbox), the profile + invite endpoints in acme-api (workstation), and the copy + welcome emails. Design: Figma "Onboarding v3".\n\n## Resources\n- Spec: \`docs/onboarding/v3.md\`\n- PR: [acme/acme-web#1322](https://github.com/acme/acme-web/pull/1322)\n- Link: [Figma — Onboarding v3](https://figma.com/)\n\n## Notes\n- Step 2 must work without a company domain (freelancers).\n- Emails go out through the existing Postmark templates.\n`
  return {
    host: 'devbox', id: 'onboarding-redesign', label: 'Onboarding redesign', path: `${H}/.local/state/fleet/stacks/onboarding-redesign.md`, cwd: '~/Code/acme-web', absCwd: `${H}/Code/acme-web`,
    created: iso(now() - 2 * D), updated: iso(now() - 25 * M), generatedAt: iso(now() - 2 * D), editedAt: iso(now() - 25 * M),
    contextLine: 'You are one of several sessions in the stack "Onboarding redesign"; shared context: ~/.local/state/fleet/stacks/onboarding-redesign.md',
    members: [
      { session: 'b2c3d4e5-2222-4000-8000-000000000001', host: 'devbox', name: 'Onboarding flow redesign', added: iso(now() - 2 * D), closed: null, live: true, status: 'busy' },
      { session: 'b2c3d4e5-2222-4000-8000-000000000002', host: 'devbox', name: 'Onboarding copy & emails', added: iso(now() - D), closed: null, live: true, status: 'idle' },
      { session: 'a1b2c3d4-1111-4000-8000-000000000007', host: 'workstation', name: 'Onboarding API endpoints', added: iso(now() - D), closed: null, live: true, status: 'busy' },
      { session: 'b2c3d4e5-2222-4000-8000-0000000000aa', host: 'devbox', name: 'Onboarding research', added: iso(now() - 2 * D), closed: iso(now() - 30 * HR), live: false },
    ],
    markdown: md, body: md.split('---\n').slice(2).join('---\n'),
    parsed: { summary: 'New 3-step onboarding for Acme web: UI in acme-web (devbox), the profile + invite endpoints in acme-api (workstation), and the copy + welcome emails. Design: Figma "Onboarding v3".', resources: [
      { kind: 'Spec', label: null, url: null, path: 'docs/onboarding/v3.md', text: 'Spec' },
      { kind: 'PR', label: 'acme/acme-web#1322', url: 'https://github.com/acme/acme-web/pull/1322', path: null, text: 'PR' },
      { kind: 'Link', label: 'Figma — Onboarding v3', url: 'https://figma.com/', path: null, text: 'Link' },
    ], notes: '- Step 2 must work without a company domain (freelancers).\n- Emails go out through the existing Postmark templates.' },
    editorUrl: `vscode://vscode-remote/ssh-remote+devbox${H}/Code/acme-web`,
  }
}

// ---------------------------------------------------------------- dormant / closed
const dormant = (host) => host !== 'workstation' ? { host, bootId: 'boot-2', dormant: [], closed: [] } : {
  host, bootId: 'boot-2',
  dormant: [{ kind: 'tmux', target: 'legacy-import', name: 'legacy-import', since: iso(now() - 9 * HR), windows: 2, panes: 3, sessions: [{ sessionId: 'dormant-legacy-import', name: 'Legacy CSV import', title: 'Legacy CSV import', cwd: `${H}/Code/acme-api` }] }],
  closed: [{ kind: 'claude', target: 'a1b2c3d4-1111-4000-8000-0000000000c1', name: 'Fix flaky auth test', since: iso(now() - 50 * M), closedAt: iso(now() - 50 * M), windows: 1, panes: 1, sessions: [{ sessionId: 'a1b2c3d4-1111-4000-8000-0000000000c1', name: 'Fix flaky auth test', title: 'Fix flaky auth test', cwd: `${H}/Code/acme-api` }] }],
}

// ---------------------------------------------------------------- jobs
function jobs(host) {
  if (host !== 'workstation') return { host, jobs: [] }
  const run = (id, ago, outcome, trig = 'schedule', sessions = []) => ({ id, trigger: trig, startedAt: now() - ago, slot: now() - ago, outcome, reason: outcome === 'skipped' ? 'previous run still working' : null, promptFile: null, sessions })
  return { host, jobs: [
    { host, name: 'dependency-audit', schedule: '0 7 * * 1-5', scheduleText: 'Weekdays at 07:00', enabled: true, dir: '~/Code/acme-web', harness: 'claude', model: 'claude-sonnet-5-5', command: null, keep: 7 * 86400,
      prompt: { file: '~/Code/ops/jobs/dependency-audit.md', abs: `${H}/Code/ops/jobs/dependency-audit.md`, root: `${H}/Code/ops`, editorUrl: 'vscode://file/x', rootEditorUrl: 'vscode://file/x' },
      nextRun: now() + 15 * HR, lastSlot: now() - 9 * HR,
      runs: [run('20261015-0700', 9 * HR, 'spawned', 'schedule', [{ tmuxSession: 'job-dependency-audit', name: 'Dependency audit', parent: null, at: now() - 9 * HR, closedAt: null }, { tmuxSession: 'fix-lodash-advisory', name: 'Fix lodash advisory', parent: 'job-dependency-audit', at: now() - 8.5 * HR, closedAt: now() - 7 * HR }]), run('20261014-0700', 33 * HR, 'spawned'), run('20261013-0700', 57 * HR, 'spawned')],
      live: [{ run: '20261015-0700', sessionId: 'a1b2c3d4-1111-4000-8000-000000000004', tmuxSession: 'job-dependency-audit', status: 'idle', title: 'Dependency audit' }] },
    { host, name: 'pr-digest', schedule: '30 8 * * *', scheduleText: 'Every day at 08:30', enabled: true, dir: '~/Code/acme-api', harness: 'claude', model: 'claude-haiku-5-5', command: null, keep: 3 * 86400,
      prompt: { inline: 'Summarise the PRs waiting on the team and post the digest to #eng-reviews.' }, nextRun: now() + 16 * HR, lastSlot: now() - 8 * HR,
      runs: [run('20261015-0830', 8 * HR, 'spawned'), run('20261014-0830', 32 * HR, 'spawned')], live: [] },
    { host, name: 'weekly-metrics', schedule: '0 9 * * 1', scheduleText: 'Mondays at 09:00', enabled: false, dir: '~/Code/analytics', harness: 'codex', model: null, command: null, keep: null,
      prompt: { inline: 'Pull last week\'s product metrics and write the weekly report.' }, nextRun: null, lastSlot: now() - 3 * D,
      runs: [run('20261012-0900', 3 * D, 'skipped')], live: [] },
  ] }
}

// ---------------------------------------------------------------- PRs
function prs(host) {
  const P = (o) => ({ owner: 'acme', draft: false, createdAt: now() - 2 * D, updatedAt: now() - 3 * HR, additions: 120, deletions: 30, changedFiles: 6, size: 'S', baseBranch: 'main', headOid: 'abc1234', reviewDecision: 'reviewRequired', checks: 'success', comments: 3, requestedReviewers: [], myReview: null, rotting: 'fresh', ...o })
  const list = [
    P({ column: 'unseenMentions', repo: 'acme-web', number: 1318, title: 'Pricing page: annual toggle', url: 'https://github.com/acme/acme-web/pull/1318', author: 'luka', headBranch: 'pricing-annual', mention: { at: now() - 50 * M, author: 'luka', url: 'https://github.com/acme/acme-web/pull/1318', excerpt: '@demo can you check the discount math here?', answered: false, seen: false }, rotting: 'fresh' }),
    P({ column: 'waitingOnMe', repo: 'acme-web', number: 1310, title: 'Checkout: retry Stripe confirmation on network errors', url: 'https://github.com/acme/acme-web/pull/1310', author: 'ana', headBranch: 'checkout-retry', size: 'M', additions: 340, deletions: 85, changedFiles: 14, createdAt: now() - 3 * D, rotting: 'stale', local: { repoPath: `${H}/Code/acme-web`, worktree: `${H}/Code/acme-web/.worktrees/pr-1310`, editorUrl: 'vscode://file/x' } }),
    P({ column: 'waitingOnMe', repo: 'billing-service', number: 207, title: 'Webhook signature v2', url: 'https://github.com/acme/billing-service/pull/207', author: 'ivo', headBranch: 'webhook-sig-v2', size: 'L', additions: 820, deletions: 240, changedFiles: 23, createdAt: now() - 6 * D, checks: 'failure', rotting: 'rotting' }),
    P({ column: 'reviewMine', repo: 'acme-api', number: 482, title: 'Fix invoice total rounding (sum in cents, round once)', url: 'https://github.com/acme/acme-api/pull/482', author: 'demo', headBranch: 'fix-invoice-rounding', size: 'S', createdAt: now() - 16 * M, checks: 'pending', requestedReviewers: ['marta'], comments: 0, local: { repoPath: `${H}/Code/acme-api`, worktree: `${H}/Code/acme-api/.worktrees/fix-invoice-rounding`, editorUrl: 'vscode://file/x' } }),
    P({ column: 'readyToMergeMine', repo: 'docs', number: 96, title: 'Docs: new API auth guide', url: 'https://github.com/acme/docs/pull/96', author: 'demo', headBranch: 'auth-guide', size: 'XS', additions: 60, deletions: 4, changedFiles: 2, reviewDecision: 'approved', rotting: 'warming' }),
    P({ column: 'draftMine', repo: 'acme-web', number: 1322, title: 'Onboarding v3 — step 1 and 2', url: 'https://github.com/acme/acme-web/pull/1322', author: 'demo', headBranch: 'onboarding-v3', draft: true, size: 'XL', additions: 1900, deletions: 610, changedFiles: 41, checks: 'pending', reviewDecision: null }),
    P({ column: 'waitingOnAuthor', repo: 'acme-api', number: 470, title: 'Bulk export: stream CSV instead of buffering', url: 'https://github.com/acme/acme-api/pull/470', author: 'petra', headBranch: 'bulk-export-stream', reviewDecision: 'changesRequested', myReview: { state: 'CHANGES_REQUESTED', at: now() - 2 * D, newCommits: false }, createdAt: now() - 8 * D, rotting: 'critical' }),
  ]
  const cols = [['unseenMentions', 'Unseen mentions'], ['waitingOnMe', 'Waiting on me'], ['reviewMine', 'Mine in review'], ['readyToMergeMine', 'Ready to merge'], ['draftMine', 'My drafts'], ['waitingOnAuthor', 'Waiting on author']]
  return { host, user: 'demo', scope: 'acme', fetchedAt: now() - 2 * M, stale: false, error: null, truncated: false, columns: cols.map(([id, title]) => ({ id, title, count: list.filter((p) => p.column === id).length })), prs: list }
}

// ---------------------------------------------------------------- usage
const usage = (host) => ({
  host, account: { uuid: 'acct-demo', email: 'demo@acme.dev', organization: 'Acme', plan: 'max', tier: 'max_5x', plan_label: 'Team · Max 5x' },
  limits: [
    { kind: 'session', group: 'session', label: 'Current session', model: null, percent: 42, severity: 'normal', resets_at: iso(now() + 2.3 * HR), active: false },
    { kind: 'weekly_all', group: 'weekly', label: 'All models', model: null, percent: 63, severity: 'normal', resets_at: iso(now() + 3.2 * D), active: false },
    { kind: 'weekly_scoped', group: 'weekly', label: 'Opus', model: 'Opus', percent: 81, severity: 'warning', resets_at: iso(now() + 3.2 * D), active: true },
  ],
  extra_usage: { enabled: true, used: 12.4, limit: 100, currency: 'USD', percent: 12 }, fetched_at: iso(now() - 40_000), stale: false, error: null,
})

// ---------------------------------------------------------------- notes
const NOTES = {
  'README.md': '---\ntitle: Team notes\ntags: [index]\n---\n# Team notes\n\nShared notes of the AI team. Start with [[projects/billing]] and the [[meetings/2026-10-14-standup]].\n',
  'projects/billing.md': '---\ntitle: Billing\ntags: [project, billing]\nowner: marta\nstatus: active\n---\n# Billing\n\nInvoices, payments and webhooks for Acme.\n\n## Current work\n- Invoice rounding — see PR acme/acme-api#482\n- Webhook retries with exponential backoff\n- Signature v2 for outgoing webhooks\n\n## Decisions\n1. Amounts are integer **cents** everywhere in the API.\n2. Round once, at the end, with banker\'s rounding.\n\nRelated: [[projects/checkout]], [[runbooks/webhooks]]\n',
  'projects/checkout.md': '---\ntitle: Checkout\ntags: [project]\n---\n# Checkout\n\nStripe Elements, 3DS and retries. Flaky e2e test tracked in [[meetings/2026-10-14-standup]].\n',
  'projects/onboarding-v3.md': '---\ntitle: Onboarding v3\ntags: [project, design]\n---\n# Onboarding v3\n\nThree steps instead of six. Freelancers without a company domain must get through step 2.\n',
  'meetings/2026-10-14-standup.md': '---\ntitle: Standup 2026-10-14\ntags: [meeting]\n---\n# Standup 2026-10-14\n\n- Checkout e2e is flaky again, a session is on it\n- Billing rounding bug reproduced\n- Release notes for October due Friday\n',
  'meetings/2026-10-07-retro.md': '# Retro 2026-10-07\n\nWhat went well: shared workstations, Board view.\n',
  'runbooks/webhooks.md': '---\ntitle: Webhooks runbook\ntags: [runbook]\n---\n# Webhooks runbook\n\n1. Check the retry queue\n2. Replay from the dashboard\n',
  'runbooks/deploy.md': '# Deploy runbook\n\n`make deploy ENV=staging` then promote.\n',
  'ideas/agent-pairing.md': '# Agent pairing\n\nTwo sessions in one stack: one writes, one reviews.\n',
}
const noteTitle = (p) => (NOTES[p].match(/title: (.*)/) || NOTES[p].match(/^# (.*)$/m) || [, p])[1]
const notesTree = (host) => ({ host, name: 'Team notes', root: '~/Code/team-notes', rootAbs: `${H}/Code/team-notes`, searchEngine: 'builtin', truncated: false, scannedAt: now(), editorUrl: 'vscode://file/x',
  files: Object.keys(NOTES).map((p, i) => ({ path: p, kind: 'markdown', size: NOTES[p].length, mtime: now() - (i + 1) * 3 * HR, title: noteTitle(p) })) })
function noteFile(host, p) {
  const text = NOTES[p] ?? '# Missing'
  const fm = text.startsWith('---\n') ? text.split('---\n')[1] : ''
  const meta = fm.split('\n').filter(Boolean).map((l) => { const [k, ...v] = l.split(': '); const val = v.join(': '); return [k, val.startsWith('[') ? val.slice(1, -1).split(', ') : val] })
  const body = fm ? text.split('---\n').slice(2).join('---\n') : text
  return { host, path: p, abs: `${H}/Code/team-notes/${p}`, kind: 'markdown', size: text.length, mtime: now() - 3 * HR, title: noteTitle(p), encrypted: false, meta, body, bodyLine: fm ? fm.split('\n').length + 2 : 1, text, editorUrl: 'vscode://file/x' }
}
function notesSearch(host, q) {
  const words = q.toLowerCase().split(/\s+/).filter(Boolean)
  const results = []
  for (const p of Object.keys(NOTES)) {
    const lines = NOTES[p].split('\n')
    const matches = []
    lines.forEach((l, i) => { const low = l.toLowerCase(); const ranges = []; for (const w of words) { let k = low.indexOf(w); if (k >= 0) ranges.push([k, k + w.length]) } if (ranges.length) matches.push({ line: i + 1, text: l, ranges }) })
    if (matches.length) results.push({ path: p, kind: 'markdown', title: noteTitle(p), mtime: now() - HR, score: matches.length, matches: matches.slice(0, 3), more: Math.max(0, matches.length - 3) })
  }
  return { host, q, engine: 'builtin', results, total: results.length, ms: 4 }
}

// ---------------------------------------------------------------- memos
const MEMOS = [
  { id: '2026-10-15-0930-billing-sync', title: 'Billing sync with Marta', created: iso(now() - 6 * HR), durationSec: 1460, status: 'done', words: 3120, preview: 'Okay so the rounding issue — Marta says finance noticed it on three invoices last week, the PDF and the API disagree by a cent…' },
  { id: '2026-10-14-1600-onboarding-review', title: 'Onboarding v3 design review', created: iso(now() - 26 * HR), durationSec: 2710, status: 'done', words: 5480, preview: 'Let\'s go through step two first. The big question is what happens for freelancers who don\'t have a company domain…' },
  { id: '2026-10-13-1100-ai-team-weekly', title: 'AI team weekly', created: iso(now() - 3 * D), durationSec: 1820, status: 'partial', words: 2050, preview: 'Agenda: shared workstation, who gets a user, Tailscale sharing, and the jobs we want to run every morning…' },
]
const memo = (id) => { const m = MEMOS.find((x) => x.id === id) ?? MEMOS[0]; return { ...m, path: `${H}/Memos/${m.id}.md`, transcript: `**Ivo:** ${m.preview}\n\n**Marta:** Right — and it only happens with three or more line items, which is why nobody caught it in tests.\n\n**Ivo:** So the fix is to sum in cents and round once. I already have a session on it on workstation.\n\n**Marta:** Good. Can we also check currencies without decimals, JPY for example?\n\n**Ivo:** Yes, I'll add that to the brief as a todo.\n\n## Action items\n- Ivo: finish the rounding PR and add JPY / KWD tests\n- Marta: confirm with finance which invoices to re-issue\n` } }

// ---------------------------------------------------------------- misc
const SETTINGS = {
  apiVersion: 1, self: 'laptop', hosts: HOSTS,
  quickReplies: [{ label: 'Continue', text: 'continue' }, { label: 'Yes', text: 'yes' }, { label: 'Run the tests', text: 'run the tests and fix what fails' }, { label: 'Commit & push', text: 'commit and push' }],
  uploads: { maxMB: 50 }, transcribe: { enabled: true }, memos: { enabled: true },
  models: [{ id: '', label: 'Default' }, { id: 'claude-opus-5-5', label: 'Opus 5.5' }, { id: 'claude-sonnet-5-5', label: 'Sonnet 5.5' }, { id: 'claude-haiku-5-5', label: 'Haiku 5.5' }],
  notes: { enabled: true, name: 'Team notes' }, stacks: { enabled: true, model: 'claude-sonnet-5-5', generate: true }, speak: { enabled: true, voice: 'alloy' }, operator: { enabled: true, hosts: ['workstation'] },
}
const HARN = (host) => ({ host, harnesses: [
  { id: 'claude', label: 'Claude Code', command: 'claude', installed: true, models: [{ id: '', label: 'Default' }, { id: 'claude-opus-5-5', label: 'Opus 5.5' }, { id: 'claude-sonnet-5-5', label: 'Sonnet 5.5' }, { id: 'claude-haiku-5-5', label: 'Haiku 5.5' }] },
  { id: 'codex', label: 'Codex', command: 'codex', installed: true, models: [{ id: '', label: 'Default' }, { id: 'gpt-5.5-codex', label: 'GPT-5.5 Codex' }] },
  { id: 'pi', label: 'pi', command: 'pi', installed: host !== 'laptop', models: [{ id: '', label: 'Default' }] },
] })
const ALLDIRS = ['acme-api', 'acme-web', 'billing-service', 'docs', 'scratch']
const spawnDirs = (host) => {
  const entries = ALLDIRS.map((l) => ({ label: l, paths: Object.fromEntries(HOSTS.filter((h) => SPAWN_DIRS[h].some((d) => d.label === l)).map((h) => [h, SPAWN_DIRS[h].find((d) => d.label === l).path])) }))
  return { host, hosts: HOSTS, spawnDirs: entries, checks: entries.map((e) => e.paths[host] ? { path: e.paths[host], resolved: e.paths[host].replace('~', H), exists: true, isDir: true } : null), offered: SPAWN_DIRS[host], limits: { maxEntries: 50, maxLabel: 40, maxPath: 300 } }
}
const R = (name, o) => ({ name, path: `~/Code/${name}`, every: 600, excluded: false, due: false, branch: 'main', upstream: 'origin/main', ahead: 0, behind: 0, dirty: 0, lastAttempt: now() - 7 * M, lastFetch: now() - 7 * M, outcome: 'current', detail: null, ...o })
const repos = (host) => ({ host, settings: { roots: ['~/Code'], every: '10m', overrides: { analytics: '12h' }, exclude: ['scratch'] }, error: null, service: { installed: true, supported: true, tickMinutes: 10 },
  repos: [R('acme-api', { outcome: 'updated', detail: 'pulled 3 commits' }), R('acme-web', { dirty: 4, outcome: 'blocked', detail: 'uncommitted changes' }), R('billing-service', { branch: 'webhook-retries', upstream: 'origin/webhook-retries', ahead: 2, outcome: 'ahead' }), R('docs'), R('search-indexer'), R('analytics', { every: 43200 }), R('team-notes'), R('scratch', { excluded: true, outcome: null, lastAttempt: null, lastFetch: null })] })
const SKILLS = [
  { name: 'code-review', description: 'Review the current diff for correctness bugs', source: 'builtin' },
  { name: 'commit', description: 'Write a commit message and commit staged changes', source: 'user' },
  { name: 'spec', description: 'Write, iterate on, and finalize a technical spec', source: 'plugin', argumentHint: '<topic>' },
  { name: 'test', description: 'Exercise a change in a real running environment', source: 'plugin' },
  { name: 'simplify', description: 'Review changed code for reuse and simplification', source: 'builtin' },
  { name: 'release-notes', description: 'Draft release notes from merged PRs', source: 'project' },
]
const FILES = {
  'src/billing/invoice_total.ts': `import { Currency, toMinor, fromMinor } from './currency'\n\nexport interface Line {\n  description: string\n  amount: number\n  quantity: number\n}\n\n/**\n * The invoice total: every line in minor units (cents), summed exactly,\n * converted back once. Matches the PDF renderer (src/pdf/totals.ts).\n */\nexport function invoiceTotal(lines: Line[], cur: Currency) {\n  const cents = lines.reduce((s, l) => s + toMinor(l.amount * l.quantity, cur), 0n)\n  return fromMinor(cents, cur)\n}\n`,
  'docs/billing/rounding.md': `# Invoice rounding\n\n**Status:** in review · PR [acme/acme-api#482](https://github.com/acme/acme-api/pull/482)\n\n## Problem\nThe API rounds each line to 2 decimals before summing; the PDF sums exact amounts and rounds once. With 3+ lines the totals drift by up to a cent.\n\n## Decision\n- Keep money in **integer minor units** (\`bigint\` cents) end to end.\n- Round **once**, at the end, with banker's rounding.\n- Currencies with 0 or 3 decimals use \`Currency.minorUnits\`.\n\n## Rollout\n1. Ship the fix behind no flag (pure bug fix).\n2. Re-issue the 3 affected invoices (finance).\n3. Add a nightly check comparing API and PDF totals.\n`,
}
const kindOf = (p) => /\.md$/.test(p) ? 'markdown' : /\.(png|jpe?g|svg)$/.test(p) ? 'image' : /\.pdf$/.test(p) ? 'pdf' : 'text'

const OPERATOR = () => ({ host: 'workstation', dir: `${H}/.local/state/fleet/operator`, session: SESSIONS().find((s) => s.operator), tmuxSession: 'operator' })

// ---------------------------------------------------------------- diff reviewer
// "Invoice rounding fix" works in its acme-api worktree and edits the PDF renderer in an
// acme-web worktree too: two checkouts, one answered thread, one waiting, one pending draft.
const ROWS = (s) => s.split('\n').map((l) => [l[0], l.slice(2)])
function hunk(oldStart, newStart, rows, section = '') {
  let o = oldStart, n = newStart
  const lines = rows.map(([t, text]) => (t === ' ' ? { t, o: o++, n: n++, text } : t === '-' ? { t, o: o++, n: null, text } : { t, o: null, n: n++, text }))
  const oldLines = lines.filter((l) => l.t !== '+').length, newLines = lines.filter((l) => l.t !== '-').length
  return { header: `@@ -${oldStart},${oldLines} +${newStart},${newLines} @@${section ? ` ${section}` : ''}`, oldStart, oldLines, newStart, newLines, section, lines }
}
function dfile(path, status, hunks) {
  const all = hunks.flatMap((h) => h.lines)
  return { path, oldPath: status === 'added' ? null : path, status, binary: false, truncated: false, additions: all.filter((l) => l.t === '+').length, deletions: all.filter((l) => l.t === '-').length, hunks }
}
const API_WT = `${H}/Code/acme-api/.worktrees/fix-invoice-rounding`
const WEB_WT = `${H}/Code/acme-web/.worktrees/invoice-pdf-totals`
const DIFF_FILES = {
  [API_WT]: [
    dfile('src/billing/invoice_total.ts', 'modified', [hunk(1, 1, ROWS(`- import { Currency, roundMoney } from './currency'
+ import { Currency, toMinor, fromMinor } from './currency'
  
  export interface Line {
    description: string
    amount: number
    quantity: number
  }
  
- export function invoiceTotal(lines: Line[], cur: Currency) {
-   return lines.reduce((s, l) => s + roundMoney(l.amount * l.quantity, cur), 0)
+ /**
+  * The invoice total: every line in minor units (cents), summed exactly,
+  * converted back once. Matches the PDF renderer (src/pdf/totals.ts).
+  */
+ export function invoiceTotal(lines: Line[], cur: Currency) {
+   const cents = lines.reduce((s, l) => s + toMinor(l.amount * l.quantity, cur), 0n)
+   return fromMinor(cents, cur)
  }`))]),
    dfile('src/billing/currency.ts', 'modified', [hunk(18, 18, ROWS(`  export function roundMoney(amount: number, cur: Currency): number {
    const f = 10 ** decimals(cur)
    return Math.round(amount * f) / f
  }
+ 
+ /** Amount → integer minor units (cents; 3 decimals for KWD / BHD). */
+ export function toMinor(amount: number, cur: Currency): bigint {
+   return BigInt(Math.round(amount * 10 ** decimals(cur)))
+ }
+ 
+ export function fromMinor(minor: bigint, cur: Currency): number {
+   return Number(minor) / 10 ** decimals(cur)
+ }`), 'export function decimals(cur: Currency) {')]),
    dfile('test/billing/invoice_total.test.ts', 'added', [hunk(0, 1, ROWS(`+ import { describe, expect, it } from 'vitest'
+ import { invoiceTotal } from '../../src/billing/invoice_total'
+ 
+ describe('invoiceTotal', () => {
+   it('matches the PDF for INV-2026-0912 (3 lines)', () => {
+     const lines = [
+       { description: 'Seats', amount: 416.665, quantity: 1 },
+       { description: 'Support', amount: 416.665, quantity: 1 },
+       { description: 'Setup', amount: 416.665, quantity: 1 },
+     ]
+     expect(invoiceTotal(lines, 'EUR')).toBe(1249.99)
+   })
+ })`))]),
  ],
  [WEB_WT]: [
    dfile('src/pdf/totals.ts', 'modified', [hunk(12, 12, ROWS(`  export function pdfTotals(inv: Invoice) {
-   const subtotal = inv.lines.reduce((s, l) => s + l.amount * l.quantity, 0)
-   return { subtotal: round(subtotal, inv.currency), tax: round(subtotal * inv.taxRate, inv.currency) }
+   // Same arithmetic as the API (acme-api src/billing/invoice_total.ts): minor units, one conversion.
+   const subtotal = invoiceTotal(inv.lines, inv.currency)
+   return { subtotal, tax: round(subtotal * inv.taxRate, inv.currency) }
  }`), 'import { round } from \'../money\'')]),
  ],
}
function diffs(host, id) {
  if (!(host === 'workstation' && id.endsWith('0001') && id.startsWith('a1'))) return { host, id, cwd: find(host, id)?.cwd ?? null, fingerprint: 'demo', unchanged: false, repos: [] }
  const repo = (root, label, branch, base) => {
    const files = DIFF_FILES[root]
    const stats = { files: files.length, additions: files.reduce((a, f) => a + f.additions, 0), deletions: files.reduce((a, f) => a + f.deletions, 0) }
    return { root, label, branch, head: '9f3c2a1', base: { ref: base, sha: '4e1d0b7c' }, tree: 'demo', fingerprint: `demo-${label}`, files, stats, truncated: false }
  }
  return {
    host, id, cwd: API_WT, fingerprint: 'demo', unchanged: false,
    repos: [repo(API_WT, '~/Code/acme-api/.worktrees/fix-invoice-rounding', 'fix/invoice-rounding', 'origin/main'), repo(WEB_WT, '~/Code/acme-web/.worktrees/invoice-pdf-totals', 'fix/invoice-pdf-totals', 'origin/main')],
  }
}
function review(host, id) {
  const t = now()
  if (!(host === 'workstation' && id.endsWith('0001') && id.startsWith('a1'))) return { host, id, threads: [], reviews: [], drafts: 0 }
  const th = (o) => ({ side: 'new', base: '4e1d0b7c', createdAt: t - 20 * M, status: 'sent', resolved: false, reviewId: null, ...o })
  return {
    host, id, drafts: 1, reviews: [],
    threads: [
      th({ id: 'c-4f1a02', root: API_WT, display: 'src/billing/invoice_total.ts', path: 'src/billing/invoice_total.ts', start: 14, end: 15, code: ['  const cents = lines.reduce((s, l) => s + toMinor(l.amount * l.quantity, cur), 0n)', '  return fromMinor(cents, cur)'], entries: [
        { body: 'Why BigInt? Number cents are exact up to 2^53, which is plenty for one invoice.', sentAt: t - 19 * M, state: 'done', replies: [{ text: 'For one invoice, yes. But `invoiceTotal` also sums **monthly statements** (up to ~40k lines) and KWD has 3 decimals, so the minor-unit sum can pass 2^53 on the largest accounts. BigInt keeps it exact and costs ~40 ns per line. I added that reason to the doc comment.', ts: t - 18 * M, final: true }] },
      ] }),
      th({ id: 'c-9b27e4', root: WEB_WT, display: '~/Code/acme-web/.worktrees/invoice-pdf-totals/src/pdf/totals.ts', path: 'src/pdf/totals.ts', start: 14, end: 15, code: ['  const subtotal = invoiceTotal(inv.lines, inv.currency)', '  return { subtotal, tax: round(subtotal * inv.taxRate, inv.currency) }'], entries: [
        { body: 'Credit notes go through `pdfTotals` too. Do they still match after this?', sentAt: t - 2 * M, state: 'working', replies: [{ text: 'Checking the credit-note fixtures in test/pdf/.', ts: t - M, final: false }] },
      ] }),
      th({ id: 'c-d03e5c', root: API_WT, display: 'test/billing/invoice_total.test.ts', path: 'test/billing/invoice_total.test.ts', start: 11, end: 11, status: 'draft', code: ["    expect(invoiceTotal(lines, 'EUR')).toBe(1249.99)"], entries: [
        { body: 'Add a KWD case (3 decimals) next to this one.', sentAt: null, state: 'draft', replies: [] },
      ] }),
    ],
  }
}

// ---------------------------------------------------------------- server
async function body(req) { const b = []; for await (const c of req) b.push(c); try { return JSON.parse(Buffer.concat(b).toString() || '{}') } catch { return {} } }
function route(method, p, q, b) {
  let m
  if (p === '/api/health') return { name: 'fleet-web', version: '0.9.0', apiVersion: 1, self: 'laptop', uptime: 86_000, now: now(), autoName: { enabled: true, intervalMinutes: 5, lastRun: { ok: true, at: now() - 2 * M, ms: 4100, renamed: [{ from: 'fw-101522', to: 'invoice-rounding-fix' }] } } }
  if (p === '/api/settings') return SETTINGS
  if (p === '/api/fleet') return fleet()
  if (p === '/api/groups' || p === '/api/groups/run' || p === '/api/groups/edit') return groups()
  if (p === '/api/operator') return OPERATOR()
  if (p === '/api/memos') return { dir: `${H}/Memos`, memos: MEMOS }
  if ((m = p.match(/^\/api\/memos\/([^/]+)$/))) return memo(decodeURIComponent(m[1]))
  if (p === '/api/transcribe') return { text: 'run the e2e suite twenty times and tell me how many fail', model: 'demo' }
  if (!(m = p.match(/^\/api\/hosts\/([^/]+)\/(.*)$/))) return null
  const host = decodeURIComponent(m[1]); const rest = m[2]
  let s
  if ((s = rest.match(/^sessions\/([^/]+)\/(.*)$/))) {
    const id = decodeURIComponent(s[1]); const what = s[2]; const sess = find(host, id)
    if (what === 'messages') { const msgs = chat(id); return { host, id, status: sess?.status ?? 'idle', backend: 'tmux', name: sess?.display_title, limit: 60, messages: msgs, total: msgs.length, truncated: false, updatedAt: sess?.updated_at, capturedAt: now() } }
    if (what === 'peek') return { host, id, backend: 'tmux', lines: 200, text: PEEK, capturedAt: now() }
    if (what === 'brief') return brief(host, id)
    if (what === 'diffs') return diffs(host, id)
    if (what === 'review') return review(host, id)
    if (what === 'complete') { const kind = q.get('kind'); return kind === 'skills' ? { host, id, kind, skills: SKILLS } : kind === 'files' ? { host, id, kind, files: ['README.md', 'docs/billing/rounding.md', 'docs/onboarding/v3.md', 'CHANGELOG.md'] } : { host, id, kind, available: true, notes: [{ ref: 'billing', path: 'projects/billing.md', title: 'Billing' }] } }
    if (what === 'files/stat') return { host, id, cwd: sess?.cwd, home: H, files: (b.paths || []).map((input) => { const rel = input.replace(/:\d+(:\d+)?$/, ''); const ok = /^(src|test|docs)\//.test(rel); return ok ? { input, path: `${sess?.cwd}/${rel}`, rel, exists: true, isFile: true, size: 2048, mtime: now() - 20 * M, kind: kindOf(rel) } : { input, path: input, exists: false } }) }
    if (what === 'stack/spawn') return { host, stack: stack(), created: true, generated: true, spawn: { name: 'x', dir: '~', tmuxSession: 'x', command: 'claude', trusted: true } }
    return { ok: true, host, id, result: 'renamed', title: b.title }
  }
  if (rest === 'harnesses') return HARN(host)
  if (rest === 'spawn-dirs') return spawnDirs(host)
  if (rest === 'jobs') return jobs(host)
  if ((s = rest.match(/^jobs\/([^/]+)$/))) return jobs(host).jobs.find((j) => j.name === decodeURIComponent(s[1]))
  if (rest === 'repos') return repos(host)
  if (rest === 'usage') return usage(host)
  if (rest === 'prs') return host === 'workstation' || host === 'laptop' ? prs(host) : prs(host)
  if (rest === 'dormant') return dormant(host)
  if (rest === 'stacks') return { host, stacks: host === 'devbox' ? [stack()] : [] }
  if (rest.startsWith('stacks/')) return stack()
  if (rest === 'notes/tree') return notesTree(host)
  if (rest === 'notes/search') return notesSearch(host, q.get('q') || '')
  if (rest === 'notes/file') return noteFile(host, q.get('path'))
  if (rest === 'autoname') return { ok: true, at: now(), renamed: [] }
  return { ok: true }
}

const MIME = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.png': 'image/png', '.json': 'application/json', '.webmanifest': 'application/manifest+json', '.woff2': 'font/woff2' }
http.createServer(async (req, res) => {
  const url = new URL(req.url, 'http://x')
  if (url.pathname === '/demo/fixture.json') {
    // The same fleet as `fleet list --json` rows, for FLEET_FIXTURE (the TUI screenshot).
    const prompts = { 'Checkout e2e flake': 'checkout.spec.ts fails ~1 in 5 runs on CI. Find out why and make it stable.', 'Rate limits spec': 'Write a spec for per-organization API rate limits.', 'October release notes': 'Draft the October release notes from merged PRs.', 'Billing webhook retries': 'Retry failed webhooks with exponential backoff.', 'Onboarding flow redesign': 'Build the new 3-step onboarding UI.', 'Search index migration': 'Migrate the search index to the new schema.' }
    const rows = SESSIONS().filter((s) => !s.operator).map((s) => (prompts[s.display_title] ? { ...s, title: prompts[s.display_title] } : s))
    res.writeHead(200, { 'content-type': 'application/json' }); return res.end(JSON.stringify(rows))
  }
  if (url.pathname.startsWith('/api/')) {
    const b = req.method === 'GET' ? {} : await body(req)
    const fr = url.pathname.match(/^\/api\/hosts\/[^/]+\/sessions\/[^/]+\/files\/raw$/)
    if (fr) { const p = url.searchParams.get('path'); const k = Object.keys(FILES).find((f) => p.endsWith(f)); res.writeHead(200, { 'content-type': 'text/plain; charset=utf-8' }); return res.end(k ? FILES[k] : `// ${p}\n`) }
    const out = route(req.method, url.pathname, url.searchParams, b)
    if (out == null) { res.writeHead(404, { 'content-type': 'application/json' }); return res.end('{"error":"not found"}') }
    res.writeHead(200, { 'content-type': 'application/json' }); return res.end(JSON.stringify(out))
  }
  let f = path.join(DIST, url.pathname === '/' ? 'index.html' : url.pathname)
  if (!fs.existsSync(f) || fs.statSync(f).isDirectory()) f = path.join(DIST, 'index.html')
  res.writeHead(200, { 'content-type': MIME[path.extname(f)] || 'application/octet-stream' })
  fs.createReadStream(f).pipe(res)
}).listen(PORT, '127.0.0.1', () => console.log(`mock on :${PORT}`))
