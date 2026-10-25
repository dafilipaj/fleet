// Colour palettes (Settings → Theme) — orthogonal to the light/dark mode (useTheme). A palette is
// `data-palette` on <html> (none = Graphite, the base tokens); its tokens live in index.css, one block per mode
// (Suite's in the generated suite/palette-suite.css).
// Pure helpers, unit-tested in palettes.test.ts (which also checks these hexes against index.css
// and the index.html pre-paint script).

export type PaletteId = 'suite' | 'graphite' | 'earth' | 'dusk'
export type PaletteMode = 'light' | 'dark'

export const PALETTE_KEY = 'fleet.palette'
/** What a viewer gets until they pick one (not stored). */
export const DEFAULT_PALETTE: PaletteId = 'suite'
/** The palette of index.css's base tokens: no `data-palette` attribute. */
export const BASE_PALETTE: PaletteId = 'graphite'

/** The three tokens a picker swatch previews: page, card, primary. `background` is also the
 *  PWA status-bar colour (meta theme-color). */
export interface Swatch {
  background: string
  card: string
  primary: string
}

export interface Palette {
  id: PaletteId
  label: string
  hint: string
  swatch: Record<PaletteMode, Swatch>
}

export const PALETTES: readonly Palette[] = [
  {
    id: 'suite',
    label: 'Suite',
    hint: 'Shared app-suite neutrals, terracotta accent',
    swatch: {
      light: { background: '#fbfbfa', card: '#f1f0ec', primary: '#a14a2a' },
      dark: { background: '#17181a', card: '#1e2022', primary: '#e39a6f' },
    },
  },
  {
    id: 'graphite',
    label: 'Graphite',
    hint: 'Neutral grey, blue accent',
    swatch: {
      light: { background: '#ffffff', card: '#f5f6f8', primary: '#2563eb' },
      dark: { background: '#0b0c0e', card: '#131519', primary: '#6aa6ff' },
    },
  },
  {
    id: 'earth',
    label: 'Earth',
    hint: 'Warm paper and walnut, terracotta accent',
    swatch: {
      light: { background: '#f7f2ea', card: '#efe7da', primary: '#a14a2a' },
      dark: { background: '#1a1512', card: '#221c17', primary: '#e39a6f' },
    },
  },
  {
    id: 'dusk',
    label: 'Dusk',
    hint: 'Soft violet-grey, violet accent',
    swatch: {
      light: { background: '#faf9fc', card: '#f2f0f7', primary: '#6547c2' },
      dark: { background: '#121119', card: '#1a1823', primary: '#b3a0ff' },
    },
  },
]

/** A stored `fleet.palette` value → a known palette (anything else → the default, Suite). */
export function parsePalette(v: string | null | undefined): PaletteId {
  return PALETTES.some((p) => p.id === v) ? (v as PaletteId) : DEFAULT_PALETTE
}

export function paletteById(id: PaletteId): Palette {
  return PALETTES.find((p) => p.id === id) ?? PALETTES[0]
}

/** meta theme-color for a palette + mode: its page background. */
export const themeColor = (id: PaletteId, mode: PaletteMode) => paletteById(id).swatch[mode].background
