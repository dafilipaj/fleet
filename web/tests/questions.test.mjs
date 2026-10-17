import { test } from 'node:test';
import assert from 'node:assert/strict';

import { normalizeQuestions, questionOutcome } from '../lib/questions.mjs';

const qs = normalizeQuestions({
  questions: [
    { question: 'Which pets do you like?', header: 'Pets', multiSelect: true, options: [{ label: 'Dog' }, { label: 'Cat' }, { label: 'Fish' }] },
    { question: 'Which city?', header: 'City', multiSelect: false, options: [{ label: 'Paris', description: 'FR' }, { label: 'Rome' }, { label: 'Oslo' }] },
  ],
});

test('normalizeQuestions keeps question, header, multiSelect and labelled options', () => {
  assert.equal(qs.length, 2);
  assert.deepEqual(qs[1].options[0], { label: 'Paris', description: 'FR' });
  assert.equal(qs[0].multiSelect, true);
  assert.equal(normalizeQuestions({}), null);
  assert.equal(normalizeQuestions({ questions: [{ question: 'Q', options: [] }] }), null);
});

test('questionOutcome: answers in question order, or declined', () => {
  const entry = { toolUseResult: { answers: { 'Which city?': 'Rome', 'Which pets do you like?': 'Dog, Fish' } } };
  assert.deepEqual(questionOutcome(qs, entry, { type: 'tool_result' }), { answers: ['Dog, Fish', 'Rome'] });
  assert.deepEqual(questionOutcome(qs, { toolUseResult: 'User rejected tool use' }, { is_error: true }), { declined: true });
});

