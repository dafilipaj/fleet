import { test } from 'node:test';
import assert from 'node:assert/strict';

import { answerSteps, normalizeQuestions, parseScreenPrompt, questionOutcome, screenShowsPrompt, validateAnswer } from '../lib/questions.mjs';

const qs = normalizeQuestions({
  questions: [
    { question: 'Which pets do you like?', header: 'Pets', multiSelect: true, options: [{ label: 'Dog' }, { label: 'Cat' }, { label: 'Fish' }] },
    { question: 'Which city?', header: 'City', multiSelect: false, options: [{ label: 'Paris', description: 'FR' }, { label: 'Rome' }, { label: 'Oslo' }] },
  ],
});
const keys = (steps) => steps.map((s) => s.key ?? `"${s.text}"`).join(' ');

test('normalizeQuestions keeps question, header, multiSelect and labelled options', () => {
  assert.equal(qs.length, 2);
  assert.deepEqual(qs[1].options[0], { label: 'Paris', description: 'FR' });
  assert.equal(qs[0].multiSelect, true);
  assert.equal(normalizeQuestions({}), null);
  assert.equal(normalizeQuestions({ questions: [{ question: 'Q', options: [] }] }), null);
});

test('answerSteps: single-select picks by arrows, a lone question needs no review', () => {
  const one = [qs[1]];
  assert.equal(keys(answerSteps(one, [{ options: [0], text: null }])), 'Enter');
  assert.equal(keys(answerSteps(one, [{ options: [2], text: null }])), 'Down Down Enter');
  assert.equal(keys(answerSteps(one, [{ options: [], text: 'Zagreb' }])), 'Down Down Down "Zagreb" Enter');
});

test('answerSteps: multi-select toggles, types, moves to Next; 2+ questions end on the review', () => {
  const steps = answerSteps(qs, [
    { options: [0, 2], text: 'hamster' },
    { options: [], text: 'Zagreb, obviously' },
  ]);
  assert.equal(
    keys(steps),
    ['Enter Down Down Enter Down "hamster" Down Enter', 'Down Down Down "Zagreb, obviously" Enter', 'Enter'].join(' '),
  );
  assert.equal(keys(answerSteps(qs, [{ options: [1], text: null }, { options: [1], text: null }])), 'Down Enter Down Down Down Enter Down Enter Enter');
});

test('validateAnswer enforces one pick for single-select, at least one for multi', () => {
  assert.equal(validateAnswer({ answers: [{ options: [0] }] }, qs).ok, false, 'one entry per question');
  assert.equal(validateAnswer({ answers: [{ options: [] }, { options: [0] }] }, qs).ok, false, 'multi needs something');
  assert.equal(validateAnswer({ answers: [{ options: [0] }, { options: [0, 1] }] }, qs).ok, false, 'single takes one');
  assert.equal(validateAnswer({ answers: [{ options: [0] }, { options: [0], text: 'x' }] }, qs).ok, false, 'single: option or text');
  assert.equal(validateAnswer({ answers: [{ options: [3] }, { options: [0] }] }, qs).ok, false, 'unknown option');
  assert.equal(validateAnswer({ answers: [{ text: 'a\nb' }, { options: [0] }] }, qs).ok, false, 'one line');
  assert.deepEqual(validateAnswer({ answers: [{ options: [2, 0, 2] }, { text: ' Zagreb ' }] }, qs), {
    ok: true,
    answers: [
      { options: [0, 2], text: null },
      { options: [], text: 'Zagreb' },
    ],
  });
});

test('questionOutcome: answers in question order, or declined', () => {
  const entry = { toolUseResult: { answers: { 'Which city?': 'Rome', 'Which pets do you like?': 'Dog, Fish' } } };
  assert.deepEqual(questionOutcome(qs, entry, { type: 'tool_result' }), { answers: ['Dog, Fish', 'Rome'] });
  assert.deepEqual(questionOutcome(qs, { toolUseResult: 'User rejected tool use' }, { is_error: true }), { declined: true });
});

test('screenShowsPrompt wants the first question, the cursor on option 1 and the prompt footer', () => {
  const screen = '←  ☐ Pets  ☐ City  ✔ Submit  →\nWhich pets do you\nlike?\n❯ 1. [ ] Dog\n  2. [ ] Cat\nEnter to select · Tab/Arrow keys to navigate';
  assert.equal(screenShowsPrompt(screen, qs), true);
  assert.equal(screenShowsPrompt(screen.replace('❯ 1.', '  1.').replace('  2.', '❯ 2.'), qs), false);
  assert.equal(screenShowsPrompt('Which city?\n❯ 1. Paris\nEnter to select', qs), false);
  assert.equal(screenShowsPrompt('❯ ', qs), false);
});

// Screens as Claude Code 2.1.310 draws them (tmux capture-pane, trailing blanks trimmed).
const RULE = '─'.repeat(40);
const singleScreen = [
  '⏺ The command ran and printed hi.',
  RULE,
  ' ☐ Color',
  '',
  'Pick a color?',
  '',
  '❯ 1. Red',
  '     Choose red',
  '  2. Blue',
  '     Choose blue',
  '  3. Type something.',
  RULE,
  '  4. Chat about this',
  '',
  'Enter to select · ↑/↓ to navigate · Esc to cancel',
].join('\n');
const multiScreen = (nuts = ' ', cursor = 1) => [
  RULE,
  '←  ☒ Fruit  ☐ Toppings  ✔ Submit  →',
  '',
  'Pick toppings?',
  '',
  `${cursor === 1 ? '❯' : ' '} 1. [${nuts}] Nuts`,
  '         Add nuts',
  '  2. [ ] Honey',
  '         Add honey',
  '  3. [ ] Type something',
  `${cursor === 4 ? '❯' : ' '}    Submit`,
  RULE,
  '  4. Chat about this',
  '',
  'Enter to select · Tab/Arrow keys to navigate · Esc to cancel',
].join('\n');
const quotedScreen = [
  '⏺ Before sending I need your call.',
  RULE,
  '←  ☐ Login  ☐ Message  ✔ Submit  →',
  '',
  '│ Testers log in with an email and a password from the local db. How',
  '│ do they get the password?',
  '│',
  '│ Options:',
  '│ - an item',
  '',
  '❯ 1. Keep the old one',
  '     Passwords stay as they are. The thread',
  "     says 'ask the owner'.",
  '  2. Set a shared one',
  '  3. Type something.',
  RULE,
  '  4. Chat about this',
  '',
  'Enter to select · Tab/Arrow keys to navigate · Esc to cancel',
].join('\n');
const reviewScreen = [
  RULE,
  '←  ☒ Fruit  ☒ Toppings  ✔ Submit  →',
  '',
  'Review your answers',
  '',
  ' ● Pick a fruit?',
  '   → Apple',
  'Ready to submit your answers?',
  '',
  '❯ 1. Submit answers',
  '  2. Cancel',
].join('\n');

test('parseScreenPrompt: a single-select question, without its "Type something" row', () => {
  const p = parseScreenPrompt(singleScreen);
  assert.match(p.id, /^screen:[0-9a-f]{12}$/);
  assert.equal(p.untouched, true);
  assert.deepEqual(p.questions, [
    { question: 'Pick a color?', header: '', multiSelect: false, options: [{ label: 'Red', description: 'Choose red' }, { label: 'Blue', description: 'Choose blue' }] },
  ]);
  assert.equal(keys(answerSteps(p.questions, [{ options: [1], text: null }])), 'Down Enter');
  assert.equal(parseScreenPrompt(singleScreen).id, p.id, 'the id is stable');
});

test('parseScreenPrompt: a multi-select tab; touched once anything is checked or the cursor moved', () => {
  const p = parseScreenPrompt(multiScreen());
  assert.equal(p.questions[0].question, 'Pick toppings?');
  assert.equal(p.questions[0].multiSelect, true);
  assert.deepEqual(p.questions[0].options.map((o) => o.label), ['Nuts', 'Honey']);
  assert.equal(p.untouched, true);
  assert.equal(parseScreenPrompt(multiScreen('✔')).untouched, false);
  assert.equal(parseScreenPrompt(multiScreen(' ', 4)).untouched, false);
  assert.equal(parseScreenPrompt(multiScreen('✔')).id, p.id, 'same question, same id');
});

test('parseScreenPrompt: quoted long questions rejoin their wrapped lines, keep paragraphs and lists', () => {
  const p = parseScreenPrompt(quotedScreen);
  assert.equal(p.questions[0].question, 'Testers log in with an email and a password from the local db. How do they get the password?\n\nOptions:\n- an item');
  assert.deepEqual(p.questions[0].options[0], { label: 'Keep the old one', description: "Passwords stay as they are. The thread says 'ask the owner'." });
  assert.equal(p.questions[0].options.length, 2);
});

test('parseScreenPrompt: the review screen is a pick between Submit and Cancel, no typing', () => {
  const p = parseScreenPrompt(reviewScreen);
  const q = p.questions[0];
  assert.equal(q.noOther, true);
  assert.equal(q.question, 'Review your answers\n● Pick a fruit?\n→ Apple\nReady to submit your answers?');
  assert.deepEqual(q.options.map((o) => o.label), ['Submit answers', 'Cancel']);
  assert.equal(validateAnswer({ answers: [{ text: 'x' }] }, p.questions).ok, false);
  assert.equal(keys(answerSteps(p.questions, validateAnswer({ answers: [{ options: [0] }] }, p.questions).answers)), 'Enter');
});

test('parseScreenPrompt: no prompt on screen → null', () => {
  assert.equal(parseScreenPrompt(''), null);
  assert.equal(parseScreenPrompt('⏺ Done.\n❯ 1. not a prompt\n'), null);
  assert.equal(parseScreenPrompt(singleScreen.replace(/\nEnter to select.*$/, '')), null, 'no footer');
});
