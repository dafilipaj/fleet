# Feature guide

A one-page tour of everything Fleet does (web desktop + mobile, Fleet.app, CLI, TUI, MCP), with a
screenshot of every screen. The screenshots come from the real web UI (`web/ui/dist`) talking to a
**demo API** with invented data — never from a live fleet, so nothing personal can end up in
them: company "Acme", user `demo` / `demo@acme.dev`, home `/Users/demo`, hosts `laptop`,
`workstation`, `devbox`.

```sh
scripts/guide/build.sh                 # everything → scripts/guide/out/ (gitignored)
scripts/guide/build.sh d-prs m-prs     # re-shoot only these; the rest of out/img stays
```

Output: `out/fleet-guide.html` (the page, Croatian, for the team) and `out/img/*.webp`.
Needs macOS, Google Chrome (`CHROME=` to override), Node ≥ 22, ImageMagick, tmux and cargo.
`FLEET_GUIDE_SETUP_URL` sets where the page's "setup i arhitektura" links point.

## Files

| file | what |
| --- | --- |
| `build.sh` | builds the UI, starts the mock, captures the TUI, shoots, converts to webp, writes `out/` |
| `mock.mjs` | the demo API: serves `web/ui/dist`, answers every route of `web/ui/src/api/client.ts` with canned data (writes change nothing); `/demo/fixture.json` is the same fleet as `fleet list --json` rows |
| `shoot.mjs` | the shot list: one entry per screenshot (name, device, steps) |
| `cdp.mjs` | a tiny Chrome DevTools Protocol driver (headless Chrome, no puppeteer): device emulation, localStorage, click by text / aria-label, keys, touch swipes, screenshots |
| `ansi.mjs` | `tmux capture-pane -e` output → HTML, for the TUI screenshot |
| `guide.html` | the page; `img/<name>.webp` names = shot names in `shoot.mjs` |

The TUI shot runs `fleet watch` with `FLEET_FIXTURE` (inert: drives nothing) in a throwaway tmux
session `fleet-test-guide`, which the script removes.

## Updating after new features

1. **Data** — a new screen or field needs demo data in `mock.mjs` (keep it invented; the repo is
   public). A route the mock doesn't know answers 404, which the UI usually shows as "not
   available" — check the shot.
2. **Shot** — add an entry to `SHOTS` in `shoot.mjs`: `d-…` desktop (1440×900 @2x, saved 1600 px
   wide), `m-…` mobile (390×844 @3x, touch, iPhone UA; saved 640 px wide). Steps use `p.go(url,
   localStorage)`, `p.click(text)` (`'=Exact'` matches whole text / aria-label), `p.key(k, { meta,
   shift })`, `p.type`, `p.swipe`, `p.eval`. Preferences are set through localStorage keys
   (`fleet.view`, `fleet.palette`, `fleet.theme`, …).
3. **Page** — add or change the section in `guide.html` and reference `img/<name>.webp`.
   `build.sh` fails when the page references an image that wasn't shot.
4. Build, look at every new image, then publish.

## Publishing (Claude Code)

The page is published as a claude.ai Artifact ("Fleet Field Guide"). Ask Claude to rebuild and
republish it: it finds the existing artifact with the Artifact tool (`action: "list"`), reads it,
then publishes `out/fleet-guide.html` with `url` set to that artifact, `root: scripts/guide/out` and
every `img/*.webp` in `files`, so the link the team has keeps working.
