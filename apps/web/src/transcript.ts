// The call's transcript, as the Mac that transcribes it sends it over
// LiveKit data messages (`TranscriptMessage` in crates/protocol).

import type { Room } from 'livekit-client';

export const TRANSCRIPT_TOPIC = 'quipcall.transcript';

export type TranscriptMessage =
  | { type: 'lines'; lines: WireLine[] }
  | { type: 'stop' }
  | { type: 'ask'; handover: boolean }
  | { type: 'answer'; accepted: boolean };

type WireLine = {
  /** Per transcriber; a later line with the same id replaces it. */
  id: number;
  /** The speaker's identity. */
  speaker: string;
  /** When they began, unix ms. */
  at_ms: number;
  text: string;
  /** Final; a draft until then. */
  done: boolean;
};

export type Line = {
  key: string;
  speaker: string;
  name: string;
  mine: boolean;
  atMs: number;
  text: string;
  done: boolean;
};

/** Lines in the order they were begun, drafts replaced by what follows them. */
export function apply(
  lines: Line[],
  by: string,
  incoming: WireLine[],
  room: Room,
): Line[] {
  const next = [...lines];
  for (const l of incoming) {
    const key = `${by}/${l.id}`;
    const at = next.findIndex((x) => x.key === key);
    if (!l.text) {
      // A final line with no text takes its draft back.
      if (at >= 0) next.splice(at, 1);
      continue;
    }
    const mine = l.speaker === room.localParticipant.identity;
    const speaker = mine ? room.localParticipant : room.remoteParticipants.get(l.speaker);
    const line: Line = {
      key,
      speaker: l.speaker,
      name: speaker?.name || (at >= 0 ? next[at].name : l.speaker),
      mine,
      atMs: l.at_ms,
      text: l.text,
      done: l.done,
    };
    if (at >= 0) next[at] = line;
    else {
      let i = next.length;
      while (i > 0 && next[i - 1].atMs > line.atMs) i--;
      next.splice(i, 0, line);
    }
  }
  return next;
}

/** Ask the Mac that transcribes to stop, for everyone. */
export async function askToStop(room: Room, identity: string) {
  const message: TranscriptMessage = { type: 'stop' };
  await room.localParticipant.publishData(new TextEncoder().encode(JSON.stringify(message)), {
    reliable: true,
    topic: TRANSCRIPT_TOPIC,
    destinationIdentities: [identity],
  });
}

export function clock(atMs: number) {
  return new Date(atMs).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}
