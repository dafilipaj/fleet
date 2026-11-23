// Voice mode for any session (#/s/:host/:id): the composer gives way to a big mic, every message
// goes out tagged VOICE_TAG, and each new final reply is read aloud. The session learns the
// rules from the brief sent when voice mode turns on; the tag on every message keeps them in
// force (and survives a compaction that drops the brief). Untagged messages: normal replies.

/** Marks a message as spoken (speech-to-text) and asks for a reply fit to be read aloud. */
export const VOICE_TAG = '[voice mode]'

/** Sent once when voice mode turns on: how to answer while it lasts. */
export const VOICE_BRIEF = `${VOICE_TAG} Voice mode is on. I'm away from the screen (driving, walking) and talk to you from my phone: my messages are speech-to-text (expect misheard words), and your final reply of each turn is read aloud to me. For every message tagged ${VOICE_TAG}, until untagged messages come back:

- Speak, don't write: two to four short plain sentences, under 400 characters. No markdown, lists, tables, code, URLs, paths or ids. Lead with the answer; offer detail instead of dumping it.
- Answer in the language I speak; keep English technical words as they are.
- Stay available to talk: act as an orchestrator. Delegate real work (reading code, edits, tests, builds, research) to background subagents (Agent tool with run_in_background, or a new fleet session for big jobs) and reply right away with what you started. Do only quick lookups yourself. When a subagent finishes, tell me its outcome in a sentence or two.
- Ask before anything hard to undo. One short question at a time, yes or no where you can.

Reply with one short sentence that you're ready.`

/** `text` as it goes out in voice mode: tagged once. */
export function tagVoice(text: string): string {
  const t = text.trim()
  if (!t || t.startsWith(VOICE_TAG)) return t
  return `${VOICE_TAG} ${t}`
}

/** localStorage key of a session's voice mode (on this device). */
export const voiceModeKey = (host: string, id: string) => `fleet.voiceMode.${host}/${id}`
