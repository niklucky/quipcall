import test from 'node:test';
import assert from 'node:assert/strict';
import { headlessEntry } from '../src/test-entry.ts';

test('headless joining is limited to explicit local development calls', () => {
  const query = new URLSearchParams('room=created&autojoin=1&mic=0&name=Bot');
  assert.deepEqual(headlessEntry(query, '127.0.0.1', true), { name: 'Bot', options: { microphone: false, camera: false } });
  assert.equal(headlessEntry(query, '127.0.0.1', false), undefined);
  assert.equal(headlessEntry(query, 'quipcall.com', true), undefined);
  query.set('desktop', 'challenge');
  assert.equal(headlessEntry(query, '127.0.0.1', true), undefined);
  query.delete('desktop'); query.delete('room');
  assert.equal(headlessEntry(query, '127.0.0.1', true), undefined);
});
