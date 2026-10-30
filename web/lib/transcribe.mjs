// Voice prompts: the composer records audio, this server sends it to OpenAI's transcription
// endpoint and answers with the text, which the UI appends to the prompt. The key comes from
// $OPENAI_SPEECH_TO_TEXT_API_KEY, else (macOS) the Keychain item of that name — never from
// the config file. No key → the feature is off (/api/settings says so, the mic is hidden).
import { HttpError } from './http.mjs';

export const KEY_NAME = 'OPENAI_SPEECH_TO_TEXT_API_KEY';
// Spoken replies: their own key when set (a restricted transcription key lacks the audio scope).
export const SPEECH_KEY_NAME = 'OPENAI_TEXT_TO_SPEECH_API_KEY';
export const DEFAULT_TRANSCRIBE_MODEL = 'gpt-4o-transcribe';
const ENDPOINT = 'https://api.openai.com/v1/audio/transcriptions';
// OpenAI's limit for one file.
export const TRANSCRIBE_MAX_BYTES = 25 * 1024 * 1024;
const HINT_MAX = 400;
// A missing key is looked up again after this long (the user may add it to the Keychain).
const MISS_TTL_MS = 60 * 1000;

// Steers the model: Latin script for Croatian (it otherwise drifts to Cyrillic), and the
// vocabulary of a coding session. The UI adds the session's own words (name, repo).
const BASE_PROMPT = 'Glasovna poruka za AI coding agenta, hrvatski ili engleski, latinica. Pojmovi: Claude, fleet, tmux, commit, PR, branch.';
// Voice memos: a conversation between people, not a prompt.
export const MEMO_PROMPT = 'Razgovor kolega o poslu i idejama, hrvatski ili engleski, latinica.';

const EXT = { 'audio/webm': 'webm', 'audio/ogg': 'ogg', 'audio/mp4': 'm4a', 'audio/x-m4a': 'm4a', 'audio/mpeg': 'mp3', 'audio/wav': 'wav', 'audio/x-wav': 'wav' };

/** `audio/webm;codecs=opus` → `audio/webm`; null for anything that is not audio we know. */
/** File extension of an audioType. */
export const audioExt = (type) => EXT[type] ?? null;

export function audioType(contentType) {
  const base = String(contentType ?? '').split(';')[0].trim().toLowerCase();
  return EXT[base] ? base : null;
}

/** The transcription prompt: the base vocabulary + the UI's hint (trimmed, one line, capped). */
export function buildPrompt(hint, base = BASE_PROMPT) {
  const extra = String(hint ?? '').replace(/\s+/g, ' ').trim().slice(0, HINT_MAX);
  return extra ? `${base} ${extra}` : base;
}

const flat = (s) => String(s ?? '').toLowerCase().replace(/[^\p{L}\p{N}]+/gu, ' ').trim();

/**
 * Whether `text` came from the prompt, not from speech. On silence (a muted mic, an empty
 * recording) the model answers with the prompt itself, or with prose strung from its terms:
 * a transcript inside the prompt, one containing the prompt's opening, or one naming three or
 * more of its hyphenated terms (session names, which nobody reads out in a row).
 */
export function promptEcho(text, prompt) {
  const t = flat(text);
  const p = flat(prompt);
  if (!t || !p) return false;
  if (t.length >= 20 && p.includes(t)) return true;
  if (t.includes(p.slice(0, 30))) return true;
  const said = String(text).toLowerCase();
  const terms = new Set(String(prompt).toLowerCase().match(/[\p{L}\p{N}]+(?:-[\p{L}\p{N}]+)+/gu) ?? []);
  let hits = 0;
  for (const term of terms) if (said.includes(term) && ++hits >= 3) return true;
  return false;
}

/** The request body as a Buffer, refusing more than `limit` bytes. */
export function readAudio(req, limit = TRANSCRIBE_MAX_BYTES) {
  const declared = Number(req.headers?.['content-length']);
  if (Number.isFinite(declared) && declared > limit) return Promise.reject(tooLarge(limit));
  return new Promise((resolve, reject) => {
    const chunks = [];
    let size = 0;
    let done = false;
    req.on('data', (chunk) => {
      if (done) return;
      size += chunk.length;
      if (size > limit) {
        done = true;
        chunks.length = 0;
        reject(tooLarge(limit));
        return;
      }
      chunks.push(chunk);
    });
    req.on('error', (err) => {
      if (done) return;
      done = true;
      reject(new HttpError(err.message, 400));
    });
    req.on('end', () => {
      if (done) return;
      done = true;
      resolve(Buffer.concat(chunks));
    });
  });
}

const tooLarge = (limit) => new HttpError(`recording too large (max ${Math.round(limit / 1024 / 1024)} MB)`, 413);

/**
 * The API key `name` (default KEY_NAME): the env var, else `security find-generic-password -s <name> -w` on macOS.
 * A hit is cached for good, a miss for a minute.
 */
export function createKeySource({ name = KEY_NAME, env = process.env, platform = process.platform, run, now = Date.now } = {}) {
  let key = null;
  let missedAt = -Infinity;
  return async function getKey() {
    if (key) return key;
    const fromEnv = env[name]?.trim();
    if (fromEnv) return (key = fromEnv);
    if (platform !== 'darwin' || !run || now() - missedAt < MISS_TTL_MS) return null;
    try {
      const { stdout } = await run('security', ['find-generic-password', '-s', name, '-w'], { timeout: 5000 });
      if (stdout.trim()) return (key = stdout.trim());
    } catch {
      /* not in the Keychain, or it is locked */
    }
    missedAt = now();
    return null;
  };
}

/**
 * @param {object} opts
 *   getKey    async () => key | null (createKeySource)
 *   model     OpenAI transcription model
 *   fetch     injectable for tests
 * @returns {{ enabled(), transcribe(req, hint): Promise<{ text, model }>, transcribeAudio(audio, type, prompt): Promise<{ text, model }> }}
 */
export function createTranscriber({ getKey, model = DEFAULT_TRANSCRIBE_MODEL, maxBytes = TRANSCRIBE_MAX_BYTES, fetch = globalThis.fetch, timeoutMs = 60 * 1000, log = () => {} }) {
  async function enabled() {
    return Boolean(await getKey());
  }

  async function transcribe(req, hint) {
    if (!(await getKey())) throw new HttpError(`voice input is off: no ${KEY_NAME} on this server`, 501);
    const type = audioType(req.headers?.['content-type']);
    if (!type) throw new HttpError('expected an audio body (webm, ogg, mp4, mp3 or wav)', 415);
    const audio = await readAudio(req, maxBytes);
    if (!audio.length) throw new HttpError('empty recording', 400);
    return transcribeAudio(audio, type, buildPrompt(hint));
  }

  /** Audio bytes of a known `type` (audioType) → text; `prompt` steers the model. */
  async function transcribeAudio(audio, type, prompt = BASE_PROMPT) {
    const key = await getKey();
    if (!key) throw new HttpError(`voice input is off: no ${KEY_NAME} on this server`, 501);
    const form = new FormData();
    form.append('model', model);
    form.append('prompt', prompt);
    form.append('response_format', 'json');
    form.append('file', new Blob([audio], { type }), `voice.${EXT[type]}`);

    let res;
    try {
      res = await fetch(ENDPOINT, {
        method: 'POST',
        headers: { authorization: `Bearer ${key}` },
        body: form,
        signal: AbortSignal.timeout(timeoutMs),
      });
    } catch (err) {
      const timedOut = err?.name === 'TimeoutError';
      throw new HttpError(timedOut ? 'transcription timed out' : `transcription service unreachable: ${err?.message ?? err}`, timedOut ? 504 : 502);
    }
    const data = await res.json().catch(() => null);
    if (!res.ok) {
      const msg = data?.error?.message || `HTTP ${res.status}`;
      log(`transcribe: OpenAI ${res.status}: ${msg}`);
      // The key's problems are this server's, not the caller's: 502, never a 401 the UI would misread.
      throw new HttpError(`transcription failed: ${msg}`, res.status === 429 ? 429 : 502);
    }
    const text = String(data?.text ?? '').trim();
    if (promptEcho(text, prompt)) {
      log(`transcribe: dropped a prompt echo (${text.length} chars) — silent recording?`);
      return { text: '', model };
    }
    return { text, model };
  }

  return { enabled, transcribe, transcribeAudio };
}

// Spoken replies (the voice operator): text → mp3 via OpenAI's speech endpoint, same key.
export const DEFAULT_SPEECH_MODEL = 'gpt-4o-mini-tts';
export const DEFAULT_VOICE = 'alloy';
const SPEECH_ENDPOINT = 'https://api.openai.com/v1/audio/speech';
// The endpoint takes at most 4096 characters.
export const SPEAK_MAX_CHARS = 4000;
const SPEECH_INSTRUCTIONS = 'Speak naturally and briskly, like a colleague giving a status update. Croatian text is Croatian (Latin script); keep English technical words in English.';

/**
 * Markdown as it should be read aloud: code blocks, tables, URLs and long ids dropped,
 * emphasis and link syntax unwrapped, list markers gone, whitespace collapsed.
 */
export function speechText(markdown) {
  let s = String(markdown ?? '');
  s = s.replace(/```[\s\S]*?(```|$)/g, ' ');
  s = s.replace(/^\s*\|.*\|\s*$/gm, ' ');
  s = s.replace(/!\[[^\]]*\]\([^)]*\)/g, ' ');
  s = s.replace(/\[([^\]]+)\]\([^)]*\)/g, '$1');
  s = s.replace(/https?:\/\/\S+/g, ' ');
  s = s.replace(/`([^`]*)`/g, '$1');
  s = s.replace(/\b[0-9a-f]{8}-[0-9a-f-]{27,}\b/gi, ' ');
  s = s.replace(/^\s{0,3}#{1,6}\s+/gm, '');
  s = s.replace(/^\s*(?:[-*+]|\d+[.)])\s+/gm, '');
  s = s.replace(/^\s*>\s?/gm, '');
  s = s.replace(/(\*\*|__|\*|_|~~)(\S(?:.*?\S)?)\1/g, '$2');
  s = s.replace(/[ \t]+/g, ' ').replace(/\s*\n\s*/g, '\n').trim();
  return s.length > SPEAK_MAX_CHARS ? `${s.slice(0, SPEAK_MAX_CHARS).replace(/\s+\S*$/, '')}…` : s;
}

/**
 * @param {object} opts
 *   getKey  async () => key | null (createKeySource)
 *   voice   OpenAI voice (config web.operator.voice)
 * @returns {{ enabled(): Promise<boolean>, voice: string, speak(text): Promise<Buffer> }}  mp3 bytes
 */
export function createSpeaker({ getKey, model = DEFAULT_SPEECH_MODEL, voice = DEFAULT_VOICE, fetch = globalThis.fetch, timeoutMs = 60 * 1000, log = () => {} }) {
  async function enabled() {
    return Boolean(await getKey());
  }

  async function speak(text) {
    const key = await getKey();
    if (!key) throw new HttpError(`spoken replies are off: no ${SPEECH_KEY_NAME} or ${KEY_NAME} on this server`, 501);
    const input = speechText(text);
    if (!input) throw new HttpError('nothing to say', 400);
    let res;
    try {
      res = await fetch(SPEECH_ENDPOINT, {
        method: 'POST',
        headers: { authorization: `Bearer ${key}`, 'content-type': 'application/json' },
        body: JSON.stringify({ model, voice, input, instructions: SPEECH_INSTRUCTIONS, response_format: 'mp3' }),
        signal: AbortSignal.timeout(timeoutMs),
      });
    } catch (err) {
      const timedOut = err?.name === 'TimeoutError';
      throw new HttpError(timedOut ? 'speech timed out' : `speech service unreachable: ${err?.message ?? err}`, timedOut ? 504 : 502);
    }
    if (!res.ok) {
      const data = await res.json().catch(() => null);
      const msg = data?.error?.message || `HTTP ${res.status}`;
      log(`speak: OpenAI ${res.status}: ${msg}`);
      throw new HttpError(`speech failed: ${msg}`, res.status === 429 ? 429 : 502);
    }
    return Buffer.from(await res.arrayBuffer());
  }

  return { enabled, voice, speak };
}
