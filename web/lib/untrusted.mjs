// Untrusted text on its way into a Claude Code prompt: what people write in Slack, on GitHub or in
// Veranda is typed into a session (a multi-line delivery is a bracketed paste). There control
// characters, escape sequences or bracketed-paste markers could act as keys: an ESC[201~ inside a
// paste would end it, and the rest would be typed as keystrokes (Enter, then `!cmd`). Every bridge
// passes such text through `sanitizeText` before it builds a prompt.

export const MAX_UNTRUSTED_TEXT = 2000;
const MAX_NAME = 60;

/**
 * Untrusted text → plain text: CR/CRLF → LF; ANSI/VT escape sequences (CSI, OSC, DCS/SOS/PM/APC, any
 * other ESC sequence, and their 8-bit C1 forms) and bracketed-paste markers removed; every other
 * C0/C1 control character and DEL removed (only `\n` and `\t` stay); bidi overrides and other
 * invisible format characters removed, U+2028/U+2029 → `\n`; at most two blank lines in a row;
 * trimmed; cut at `max` chars (saying so).
 */
export function sanitizeText(text, max = MAX_UNTRUSTED_TEXT) {
  let t = String(text ?? '');
  t = t
    .replace(/\r\n?/g, '\n')
    .replace(/[\u2028\u2029]/g, '\n')
    // 7-bit escape sequences: CSI, OSC (to BEL or ST), DCS / SOS / PM / APC (to ST), then any ESC + one char.
    .replace(/\x1b\[[0-?]*[ -/]*[@-~]?/g, '')
    .replace(/\x1b\][\s\S]*?(?:\x07|\x1b\\|$)/g, '')
    .replace(/\x1b[PX^_][\s\S]*?(?:\x1b\\|$)/g, '')
    .replace(/\x1b[\s\S]?/g, '')
    // 8-bit forms: CSI (0x9B), OSC (0x9D), DCS/SOS/PM/APC (0x90, 0x98, 0x9E, 0x9F) to ST (0x9C) or BEL.
    .replace(/\x9b[0-?]*[ -/]*[@-~]?/g, '')
    .replace(/[\x90\x98\x9d\x9e\x9f][\s\S]*?(?:\x07|\x9c|$)/g, '')
    // Bracketed-paste markers whose ESC was already gone (or typed as text).
    .replace(/\[20[01]~/g, '')
    // Every other control character but \n and \t; DEL; C1.
    .replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f]/g, '')
    // Invisible format characters that could hide or reorder text: zero-width, bidi, BOM.
    .replace(/[\u200b-\u200f\u202a-\u202e\u2060-\u2064\u2066-\u206f\ufeff]/g, '')
    .replace(/[ \t]+$/gm, '')
    .replace(/\n{3,}/g, '\n\n')
    .trim();
  if (t.length > max) t = `${t.slice(0, max)} … (cut, ${t.length - max} more chars)`;
  return t;
}

/** A person's name or username for one line: sanitised, whitespace collapsed, short. */
export function sanitizeName(name) {
  const n = sanitizeText(name, 500).replace(/\s+/g, ' ').trim();
  return n.length > MAX_NAME ? `${n.slice(0, MAX_NAME - 1)}…` : n;
}

