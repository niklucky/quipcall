import type { CallRoom, Principal, Entry } from './api';
import type { JoinOptions } from './media';

export type JoinRequest = {
  destination: 'browser' | 'mac' | 'approve';
  roomId: string;
  invitation: string;
  desktopId: string;
  desktopCode: string;
  name: string;
  title: string;
  options: JoinOptions;
};
export type JoinEffects = {
  identify: (name: string, current: () => boolean) => Promise<Principal>;
  redeem: (token: string) => Promise<CallRoom>;
  accepted: (room: CallRoom) => void;
  stopPreview: () => void;
  approve: (id: string, code: string) => Promise<unknown>;
  approved: () => void;
  openMac: (roomId: string) => void;
  join: (roomId: string, name: string, options: JoinOptions, title: string) => Promise<void>;
};

/** Mac approval authenticates an identity; the Mac checks room admission
 * afterwards. A destination link must not gate or consume that approval. */
export async function loadJoinEntry(
  target: Pick<JoinRequest, 'desktopId' | 'roomId' | 'invitation'>,
  check: (body: { invite: string } | { room: string }) => Promise<Entry>,
): Promise<Entry> {
  if (target.desktopId || (!target.invitation && !target.roomId)) return { state: 'ready' };
  return check(target.invitation ? { invite: target.invitation } : { room: target.roomId });
}

/** Navigation cancels the continuation of an action, including a response that
 * arrives after teardown. Already-sent requests may finish on the server, but
 * must never trigger another request, change the route or join a call. */
export class JoinFlow {
  private generation = 0;
  cancel() { ++this.generation; }
  async run(input: JoinRequest, effects: JoinEffects): Promise<void> {
    const generation = ++this.generation;
    const current = () => generation === this.generation;
    // Capture the entry and media choices before the first asynchronous step.
    const request = { ...input, options: { ...input.options } };
    try {
      const principal = await effects.identify(request.name, current);
      if (!current()) return;
      if (request.invitation && request.destination !== 'approve') {
        const room = await effects.redeem(request.invitation);
        if (!current()) return;
        request.roomId = room.id;
        request.title = room.name;
        effects.accepted(room);
        if (!current()) return;
      }
      effects.stopPreview();
      if (!current()) return;
      if (request.destination === 'approve') {
        await effects.approve(request.desktopId, request.desktopCode);
        if (!current()) return;
        effects.approved();
      } else if (request.destination === 'mac') {
        effects.openMac(request.roomId);
      } else {
        await effects.join(request.roomId, principal.name, request.options, request.title);
        if (!current()) return;
      }
    } catch (error) {
      if (current()) throw error;
    }
  }
}
