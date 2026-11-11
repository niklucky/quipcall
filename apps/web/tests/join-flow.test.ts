import test from 'node:test';
import assert from 'node:assert/strict';
import { JoinFlow, type JoinEffects, type JoinRequest } from '../src/join-flow.ts';
import type { Principal, CallRoom } from '../src/api.ts';
const principal: Principal = { id: 'guest', kind: 'guest', name: 'Guest', email: null, admin: false };
const room: CallRoom = { id: 'claimed-room', name: 'Project', kind: 'private', owner: 'owner', guest: null, closed: false };
function deferred<T>() { let resolve!: (v: T) => void; const promise = new Promise<T>(r => { resolve = r; }); return { resolve, promise }; }
function setup(destination: JoinRequest['destination'] = 'browser') {
  const calls: string[] = [];
  const input: JoinRequest = { destination, roomId: '', invitation: 'invite', desktopId: 'proof', desktopCode: 'ABC12345', name: 'Guest', title: 'Project', options: { microphone: false, camera: true, videoDeviceId: 'camera-one' } };
  const effects: JoinEffects = {
    identify: async () => principal,
    redeem: async token => { calls.push(`redeem:${token}`); return room; },
    accepted: value => { calls.push(`accepted:${value.id}`); },
    stopPreview: () => { calls.push('stop'); },
    approve: async (id, code) => { calls.push(`approve:${id}:${code}`); },
    approved: () => { calls.push('approved'); },
    openMac: id => { calls.push(`open:${id}`); },
    join: async (id, name, options) => { calls.push(`join:${id}:${name}:${options.videoDeviceId}`); },
  };
  return { input, effects, calls, flow: new JoinFlow() };
}
for (const destination of ['browser', 'mac', 'approve'] as const) {
  test(`leaving during identification prevents all ${destination} continuation effects`, async () => {
    const { input, effects, calls, flow } = setup(destination);
    const pending = deferred<Principal>(); effects.identify = () => pending.promise;
    const run = flow.run(input, effects);
    flow.cancel(); // The lobby cancels on navigation, and Join cancels on teardown.
    pending.resolve(principal); await run;
    assert.deepEqual(calls, []);
  });
}
test('an in-flight redemption cannot redirect or join after cancellation', async () => {
  const { input, effects, calls, flow } = setup();
  const started = deferred<void>(), pending = deferred<CallRoom>();
  effects.redeem = () => { started.resolve(); return pending.promise; };
  const run = flow.run(input, effects); await started.promise;
  flow.cancel(); pending.resolve(room); await run;
  assert.deepEqual(calls, []);
});
test('an in-flight desktop approval cannot update an abandoned page', async () => {
  const { input, effects, calls, flow } = setup('approve'); input.invitation = '';
  const started = deferred<void>(), pending = deferred<void>();
  effects.approve = () => { started.resolve(); return pending.promise; };
  const run = flow.run(input, effects); await started.promise;
  flow.cancel(); pending.resolve(); await run;
  assert.deepEqual(calls, ['stop']);
});
test('a current attempt completes with its original entry and device choices', async () => {
  const { input, effects, calls, flow } = setup();
  const pending = deferred<Principal>(); effects.identify = () => pending.promise;
  const run = flow.run(input, effects);
  input.invitation = 'a-different-invitation'; input.options.videoDeviceId = 'other-camera';
  pending.resolve(principal); await run;
  assert.deepEqual(calls, ['redeem:invite', 'accepted:claimed-room', 'stop', 'join:claimed-room:Guest:camera-one']);
});
