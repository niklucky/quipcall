export type JoinOptions = {
  microphone: boolean;
  camera: boolean;
  audioDeviceId?: string;
  videoDeviceId?: string;
};
export type PreviewState = {
  audio?: MediaStreamTrack;
  video?: MediaStreamTrack;
  audioBusy: boolean;
  videoBusy: boolean;
  audioError: string;
  videoError: string;
};
type Kind = 'audio' | 'video';

export function mediaError(error: unknown, kind: Kind): string {
  const name = kind === 'audio' ? 'Microphone' : 'Camera';
  switch ((error as { name?: string })?.name) {
    case 'NotAllowedError': return `${name} access is blocked. Allow it in your browser’s site settings, then try again. You can still join with it off.`;
    case 'NotFoundError': return `No ${name.toLowerCase()} was found. Connect one or join with it off.`;
    case 'NotReadableError': return `${name} is busy in another app. Close it there and try again, or join with it off.`;
    case 'OverconstrainedError': return `That ${name.toLowerCase()} is unavailable. Choose another device.`;
    default: return `${name} could not start. Try again or join with it off.`;
  }
}

/** Owns preview tracks only. A cancelled or stale permission request must not
 * reactivate a device after the user switches it off or leaves the page. */
export class MediaPreview {
  state: PreviewState = { audioBusy: false, videoBusy: false, audioError: '', videoError: '' };
  private generation = { audio: 0, video: 0 };
  private disposed = false;
  private devices: Pick<MediaDevices, 'getUserMedia'> | undefined;
  private changed: (state: PreviewState) => void;
  constructor(devices: Pick<MediaDevices, 'getUserMedia'> | undefined, changed: (state: PreviewState) => void) { this.devices = devices; this.changed = changed; }
  private notify() { this.changed({ ...this.state }); }
  async set(kind: Kind, enabled: boolean, deviceId?: string) {
    if (this.disposed) return;
    const generation = ++this.generation[kind];
    this.state[`${kind}Error`] = '';
    this.state[`${kind}Busy`] = enabled;
    if (!enabled) {
      this.state[kind]?.stop();
      this.state[kind] = undefined;
      this.notify();
      return;
    }
    this.notify();
    let stream: MediaStream | undefined;
    try {
      if (!this.devices) throw new Error('Media unavailable');
      stream = await this.devices.getUserMedia({
        audio: kind === 'audio' ? { ...(deviceId ? { deviceId: { exact: deviceId } } : {}), echoCancellation: true, noiseSuppression: true } : false,
        video: kind === 'video' ? { ...(deviceId ? { deviceId: { exact: deviceId } } : {}), width: { ideal: 1280 }, height: { ideal: 720 } } : false,
      });
      if (this.disposed || generation !== this.generation[kind]) { stream.getTracks().forEach(t => t.stop()); return; }
      const track = kind === 'audio' ? stream.getAudioTracks()[0] : stream.getVideoTracks()[0];
      if (!track) throw new Error('Missing track');
      stream.getTracks().filter(t => t !== track).forEach(t => t.stop());
      this.state[kind]?.stop();
      this.state[kind] = track;
      track.onended = () => {
        if (this.state[kind] !== track || this.disposed) return;
        this.state[kind] = undefined;
        this.state[`${kind}Error`] = `${kind === 'audio' ? 'Microphone' : 'Camera'} disconnected. Choose a device or join with it off.`;
        this.notify();
      };
    } catch (error) {
      stream?.getTracks().forEach(t => t.stop());
      if (this.disposed || generation !== this.generation[kind]) return;
      this.state[`${kind}Error`] = mediaError(error, kind);
    } finally {
      if (!this.disposed && generation === this.generation[kind]) {
        this.state[`${kind}Busy`] = false;
        this.notify();
      }
    }
  }
  stop() {
    for (const kind of ['audio', 'video'] as const) {
      ++this.generation[kind];
      this.state[kind]?.stop();
      this.state[kind] = undefined;
      this.state[`${kind}Busy`] = false;
    }
    this.notify();
  }
  dispose() { this.stop(); this.disposed = true; }
}
