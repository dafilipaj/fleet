// Every screenshot of the feature guide, against the demo API (mock.mjs) on GUIDE_BASE.
// Desktop: 1440×900 @2x (the board 1800 wide), mobile: 390×844 @3x with touch + iPhone UA.
// `node shoot.mjs [name…]` shoots only those; a failed step is reported and the rest go on.
// Names here are the files guide.html references (img/<name>.webp) — keep both in step.
import { launch, BASE as U } from './cdp.mjs'

const INV = '#/s/workstation/a1b2c3d4-1111-4000-8000-000000000001' // Invoice rounding fix: the rich chat + brief
const CHK = '#/s/workstation/a1b2c3d4-1111-4000-8000-000000000002' // waiting on a permission
const RL = '#/s/workstation/a1b2c3d4-1111-4000-8000-000000000003'  // waiting on an AskUserQuestion
const ONB = '#/s/devbox/b2c3d4e5-2222-4000-8000-000000000001'      // in a stack across two hosts
const MUTE = { 'fleet.operator.speak': 'false' }                    // no read-aloud toast on the operator
const focus = `document.querySelector('textarea[aria-label=Message]').focus()`
const scrollTo = (t) => `(() => { const e = [...document.querySelectorAll('*')].find((e) => e.children.length === 0 && e.textContent?.trim() === ${JSON.stringify(t)}); e?.scrollIntoView({ block: 'start' }); return !!e })()`
const scrollAll = (last) => `(() => { const a = [...document.querySelectorAll('*')].filter((e) => e.scrollHeight > e.clientHeight + 50 && getComputedStyle(e).overflowY !== 'visible'); ${last ? 'a.slice(-1)' : 'a'}.forEach((e) => { e.scrollTop = 1e5 }) })()`

const REV = '#/review/workstation/a1b2c3d4-1111-4000-8000-000000000001' // its diff reviewer: two worktrees, threads
const UNI = { 'fleet.diffView': 'unified' }
// Scroll the diff to a file's card (by the end of its path).
const toCard = (end, block = 'start') => `(() => { const e = [...document.querySelectorAll('[data-file-key]')].find((e) => e.dataset.fileKey.endsWith(${JSON.stringify(end)})); e?.scrollIntoView({ block: ${JSON.stringify(block)} }); return !!e })()`
// Select the code of new lines a..b in a file's card, as a mouse drag would, and let the reviewer see it.
const selectLines = (end, a, b) => `(() => {
  const card = [...document.querySelectorAll('[data-file-key]')].find((e) => e.dataset.fileKey.endsWith(${JSON.stringify(end)}))
  const rows = [...card.querySelectorAll('[data-line]')].filter((r) => r.dataset.n && r.dataset.side !== 'old')
  const row = (n) => rows.find((r) => r.dataset.n === String(n)).lastElementChild
  const range = document.createRange(); range.setStart(row(${a}).firstChild, 2); range.setEnd(row(${b}).firstChild, row(${b}).textContent.length)
  const sel = window.getSelection(); sel.removeAllRanges(); sel.addRange(range)
  card.closest('.overflow-y-auto').dispatchEvent(new MouseEvent('mouseup', { bubbles: true }))
  return true })()`

const DESKTOP = { w: 1440, h: 900 }
const MOBILE = { w: 390, h: 844, mobile: true, scale: 3 }
const SHOTS = [
  // desktop
  ['d-session', DESKTOP, async (p) => { await p.go(U + INV) }],
  ['d-palette', DESKTOP, async (p) => { await p.go(U + INV); await p.key('k', { meta: true }); await p.type('onb') }],
  ['d-help', DESKTOP, async (p) => { await p.go(U + INV); await p.key('?', { meta: true, shift: true }) }],
  ['d-new', DESKTOP, async (p) => { await p.go(U + INV); await p.key('o', { meta: true, shift: true, ms: 1200 }) }],
  ['d-question', DESKTOP, async (p) => { await p.go(U + RL) }],
  ['d-board-wide', { w: 1800, h: 520 }, async (p) => { await p.go(U + '#/', { 'fleet.view': 'board' }) }],
  ['d-board-flyout', DESKTOP, async (p) => { await p.go(U + INV, { 'fleet.view': 'board' }) }],
  ['d-file', DESKTOP, async (p) => { await p.go(U + INV); await p.click('=docs/billing/rounding.md', { ms: 1500 }) }],
  ['d-stack', DESKTOP, async (p) => { await p.go(U + ONB); await p.click('StackBrief', { ms: 1500 }) }],
  ['d-prs', DESKTOP, async (p) => { await p.go(U + '#/prs') }],
  ['d-jobs', DESKTOP, async (p) => { await p.go(U + '#/bots'); await p.click('dependency-audit', { ms: 1500 }) }],
  ['d-notes', DESKTOP, async (p) => { await p.go(U + '#/notes/devbox/projects/billing.md') }],
  ['d-notes-search', DESKTOP, async (p) => { await p.go(U + '#/notes/devbox'); await p.key('f', { meta: true }); await p.type('rounding', 1200) }],
  ['d-usage', DESKTOP, async (p) => { await p.go(U + '#/usage') }],
  ['d-settings', DESKTOP, async (p) => { await p.go(U + '#/settings') }],
  ['d-settings-repos', DESKTOP, async (p) => { await p.go(U + '#/settings'); await p.eval(scrollTo('Git repos')); await p.sleep(1200) }],
  ['d-settings-dirs', DESKTOP, async (p) => { await p.go(U + '#/settings'); await p.eval(scrollTo('Start directories')); await p.sleep(1200) }],
  ['d-memo', DESKTOP, async (p) => { await p.go(U + '#/memos/2026-10-15-0930-billing-sync') }],
  ['d-operator', DESKTOP, async (p) => { await p.go(U + '#/operator', MUTE) }],
  ['d-complete', DESKTOP, async (p) => { await p.go(U + INV); await p.eval(focus); await p.type('/', 1200) }],
  ['d-undo', DESKTOP, async (p) => { await p.go(U + INV); await p.eval(focus); await p.type('Also add a test for KWD (3 decimals).'); await p.key('Enter', { ms: 900 }) }],
  ['d-scratch', DESKTOP, async (p) => { await p.go(U + INV, { 'fleet.scratchpad': 'TODO today\n- review acme-web#1310\n- answer rate limits question\n- ship invoice fix before 16:00\n\nfleet -H workstation enter invoice' }); await p.key('s', { meta: true, shift: true, ms: 1000 }) }],
  ['d-dusk', { ...DESKTOP, dark: true }, async (p) => { await p.go(U + INV, { 'fleet.palette': 'dusk', 'fleet.theme': 'dark' }) }],
  ['d-earth', DESKTOP, async (p) => { await p.go(U + '#/', { 'fleet.palette': 'earth', 'fleet.view': 'board' }) }],
  ['d-dormant', DESKTOP, async (p) => { await p.go(U + '#/'); await p.eval(scrollAll(false)); await p.sleep(700) }],
  ['d-details-bottom', DESKTOP, async (p) => { await p.go(U + INV); await p.eval(scrollAll(true)); await p.sleep(700) }],
  ['d-review', DESKTOP, async (p) => { await p.go(U + REV, UNI); await p.eval(toCard('invoice_total.ts')); await p.sleep(800) }],
  ['d-review-select', DESKTOP, async (p) => { await p.go(U + REV, UNI); await p.eval(toCard('currency.ts')); await p.sleep(500); await p.eval(selectLines('currency.ts', 23, 25)); await p.sleep(800) }],
  ['d-review-compose', DESKTOP, async (p) => { await p.go(U + REV, UNI); await p.eval(toCard('currency.ts')); await p.sleep(500); await p.eval(selectLines('currency.ts', 23, 25)); await p.sleep(600); await p.click('Comment on lines', { sel: 'button' }); await p.type('Math.round on a float can still be off for x.xx5 amounts. Use a decimal string conversion here?', 900) }],
  ['d-review-split', { ...DESKTOP, dark: true }, async (p) => { await p.go(U + REV, { 'fleet.diffView': 'split', 'fleet.reviewChat': 'false', 'fleet.theme': 'dark' }); await p.eval(toCard('totals.ts', 'center')); await p.sleep(800) }],
  // mobile
  ['m-list', MOBILE, async (p) => { await p.go(U + '#/') }],
  ['m-filters', MOBILE, async (p) => { await p.go(U + '#/'); await p.swipe(195, 40, 195, 200) }],
  ['m-board', MOBILE, async (p) => { await p.go(U + '#/', { 'fleet.view': 'board' }) }],
  ['m-more', MOBILE, async (p) => { await p.go(U + '#/'); await p.click('More') }],
  ['m-new', MOBILE, async (p) => { await p.go(U + '#/'); await p.click('New session', { ms: 1200 }) }],
  ['m-chat', MOBILE, async (p) => { await p.go(U + INV) }],
  ['m-chips', MOBILE, async (p) => { await p.go(U + CHK); await p.click('Show quick replies', { ms: 900 }) }],
  ['m-question', MOBILE, async (p) => { await p.go(U + RL) }],
  ['m-details', MOBILE, async (p) => { await p.go(U + INV); await p.click('=More', { ms: 900 }) }],
  ['m-stack', MOBILE, async (p) => { await p.go(U + ONB); await p.click('StackBrief', { ms: 1500 }) }],
  ['m-operator', MOBILE, async (p) => { await p.go(U + '#/operator', MUTE) }],
  ['m-memos', MOBILE, async (p) => { await p.go(U + '#/memos') }],
  ['m-prs', MOBILE, async (p) => { await p.go(U + '#/prs') }],
  ['m-jobs', MOBILE, async (p) => { await p.go(U + '#/bots') }],
  ['m-notes', MOBILE, async (p) => { await p.go(U + '#/notes/devbox/projects/billing.md') }],
  ['m-review', MOBILE, async (p) => { await p.go(U + REV, UNI); await p.eval(toCard('invoice_total.ts')); await p.sleep(800) }],
  ['m-usage', MOBILE, async (p) => { await p.go(U + '#/usage') }],
  ['m-earth-dark', { ...MOBILE, dark: true }, async (p) => { await p.go(U + '#/', { 'fleet.palette': 'earth', 'fleet.theme': 'dark' }) }],
]

const only = process.argv.slice(2)
const p = await launch()
let failed = 0
try {
  for (const [name, device, run] of SHOTS) {
    if (only.length && !only.includes(name)) continue
    try { await p.setup(device); await run(p); await p.shot(name) } catch (e) { failed++; console.log('FAIL', name, e.message) }
  }
  // The TUI: an ANSI capture turned into HTML by build.sh (GUIDE_TUI_HTML), cropped to the <pre>.
  if (process.env.GUIDE_TUI_HTML && (!only.length || only.includes('tui'))) {
    await p.setup({ w: 1120, h: 640 }); await p.go(`file://${process.env.GUIDE_TUI_HTML}`)
    const [width, height] = await p.eval(`(() => { const b = document.querySelector('pre').getBoundingClientRect(); return [b.width, b.height] })()`)
    await p.shot('tui', { x: 0, y: 0, width, height })
  }
} finally { p.close() }
process.exit(failed ? 1 : 0)
