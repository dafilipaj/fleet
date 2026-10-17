// Claude Code's AskUserQuestion prompt, read from the transcript. Answering it is the CLI's
// (`fleet answer --question`, through lib/fleet-cli.mjs): it checks the form on screen, every
// key and the review screen, under the session's typing lock.

export const QUESTION_TOOL = 'AskUserQuestion';

const str = (v) => (typeof v === 'string' ? v : '');

/** The tool input's questions, normalized; null when it isn't a usable question list. */
export function normalizeQuestions(input) {
  const raw = input && Array.isArray(input.questions) ? input.questions : null;
  if (!raw?.length) return null;
  const out = [];
  for (const q of raw) {
    if (!q || typeof q !== 'object' || !str(q.question).trim()) return null;
    const options = Array.isArray(q.options)
      ? q.options.filter((o) => o && str(o.label).trim()).map((o) => ({ label: str(o.label), description: str(o.description) }))
      : [];
    if (!options.length) return null;
    out.push({ question: str(q.question), header: str(q.header), multiSelect: q.multiSelect === true, options });
  }
  return out;
}

/** Plain-markdown rendering, for readers that don't know `kind: 'question'`. */
export function questionsText(questions) {
  return questions
    .map((q) => [`**${q.header ? `${q.header}: ` : ''}${q.question}**`, ...q.options.map((o) => `- ${o.label}`)].join('\n'))
    .join('\n\n');
}

/**
 * What became of the prompt, from its tool_result entry: `{ answers: (string|null)[] }` aligned
 * with `questions`, or `{ declined: true }` (Esc, "Chat about this", interrupted).
 */
export function questionOutcome(questions, entry, block) {
  const answers = entry?.toolUseResult?.answers;
  if (!block?.is_error && answers && typeof answers === 'object') {
    return { answers: questions.map((q) => (typeof answers[q.question] === 'string' ? answers[q.question] : null)) };
  }
  return { declined: true };
}
