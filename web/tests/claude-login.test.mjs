import { test } from 'node:test';
import assert from 'node:assert/strict';
import { claudeFailure, isLoginError, LOGIN_HINT } from '../lib/claude-login.mjs';

test('a dead login (printed on stdout) gets the `claude auth login` hint', () => {
  const err = Object.assign(new Error('Command failed: claude -p'), {
    stderr: '',
    stdout: 'Failed to authenticate: this login reached its time limit and cannot be renewed.\n',
  });
  const why = claudeFailure(err);
  assert.match(why, /^`claude -p` failed: Failed to authenticate/);
  assert.ok(why.endsWith(LOGIN_HINT));
  assert.match(LOGIN_HINT, /claude auth login/);
});

test('other failures keep their last stderr line and no hint', () => {
  const err = Object.assign(new Error('x'), { stderr: 'warn\nAPI Error: 529 overloaded\n', stdout: '' });
  assert.equal(claudeFailure(err), '`claude -p` failed: API Error: 529 overloaded');
  assert.equal(isLoginError('API Error: 529 overloaded'), false);
  assert.equal(isLoginError('Not logged in · Please run /login'), true);
});
