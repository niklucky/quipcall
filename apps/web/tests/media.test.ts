import test from 'node:test';
import assert from 'node:assert/strict';
import { MediaPreview } from '../src/media.ts';

function fixture(kind = 'video') {
  const track = { kind, stopped: false, onended: null as null | (() => void), stop() { this.stopped = true; } };
  const stream = { getTracks: () => [track], getAudioTracks: () => kind === 'audio' ? [track] : [], getVideoTracks: () => kind === 'video' ? [track] : [] } as unknown as MediaStream;
  return { track, stream };
}
function deferred() { let resolve!: (s: MediaStream) => void; const promise = new Promise<MediaStream>(r => { resolve = r; }); return { resolve, promise }; }

test('a cancelled permission request cannot turn a device back on', async () => {
  const pending = deferred(); const { track, stream } = fixture(); let calls = 0;
  const preview = new MediaPreview({ getUserMedia: () => { ++calls; return pending.promise; } }, () => {});
  assert.equal(calls, 0);
  const enable = preview.set('video', true);
  await preview.set('video', false);
  pending.resolve(stream); await enable;
  assert.equal(track.stopped, true); assert.equal(preview.state.video, undefined); assert.equal(preview.state.videoBusy, false);
});
test('latest device selection wins and releases stale streams', async () => {
  const one = deferred(), two = deferred(); const old = fixture(), latest = fixture(); let calls = 0;
  const preview = new MediaPreview({ getUserMedia: () => ++calls === 1 ? one.promise : two.promise }, () => {});
  const a = preview.set('video', true, 'one'), b = preview.set('video', true, 'two');
  two.resolve(latest.stream); await b; one.resolve(old.stream); await a;
  assert.equal(old.track.stopped, true); assert.equal(preview.state.video, latest.track);
  preview.dispose(); assert.equal(latest.track.stopped, true);
});
test('leaving the preview stops live and pending media', async () => {
  const pending = deferred(), audio = fixture('audio'), video = fixture();
  const preview = new MediaPreview({ getUserMedia: c => c.audio ? Promise.resolve(audio.stream) : pending.promise }, () => {});
  await preview.set('audio', true); const enable = preview.set('video', true);
  preview.dispose(); pending.resolve(video.stream); await enable;
  assert.equal(audio.track.stopped, true); assert.equal(video.track.stopped, true);
  assert.equal(preview.state.audio, undefined); assert.equal(preview.state.video, undefined);
});
test('denied permission leaves the device off with useful recovery guidance', async () => {
  const preview = new MediaPreview({ getUserMedia: async () => { throw { name: 'NotAllowedError' }; } }, () => {});
  await preview.set('audio', true);
  assert.equal(preview.state.audio, undefined); assert.equal(preview.state.audioBusy, false);
  assert.match(preview.state.audioError, /site settings/); assert.match(preview.state.audioError, /join with it off/);
});
