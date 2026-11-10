// The call's transcript, as the Mac that transcribes it sends it over
// LiveKit data messages (`TranscriptMessage` in crates/protocol).

import type { Participant, Room } from 'livekit-client';

import { TRANSCRIBING_ATTRIBUTE } from './call';

export const TRANSCRIPT_TOPIC = 'quipcall.transcript';
/**
 * How long after a Mac stops transcribing its last lines are still taken:
 * its claim goes first, then the last words. `LAST_WORDS_MS` in crates/protocol.
 */
const LAST_WORDS_MS = 15_000;

export type TranscriptMessage =
  | { type: 'lines'; lines: WireLine[] }
  | { type: 'stop' }
  | { type: 'ask'; handover: boolean; claim?: number }
  | { type: 'answer'; accepted: boolean };

type WireLine = {
  /** Per transcriber (`by`); a later line with the same id replaces it. */
  id: number;
  /** The speaker's identity. */
  speaker: string;
  /** When they began, unix ms. */
  at_ms: number;
  text: string;
  /** Final; a draft until then. */
  done: boolean;
  /** The Mac that transcribed it, whoever relays it. Older apps leave it out: then it's the sender's. */
  by?: string;
};

const isWireLine = (x: unknown): x is WireLine => {
  if (typeof x !== 'object' || x === null) return false;
  const l = x as Record<string, unknown>;
  return (
    typeof l.id === 'number' &&
    typeof l.speaker === 'string' &&
    typeof l.at_ms === 'number' &&
    typeof l.text === 'string' &&
    typeof l.done === 'boolean' &&
    (l.by === undefined || typeof l.by === 'string')
  );
};

/** The lines in a message, if it brings any. Anything malformed is left out. */
export function linesIn(payload: Uint8Array): WireLine[] {
  try {
    const message = JSON.parse(new TextDecoder().decode(payload)) as unknown;
    if (typeof message !== 'object' || message === null) return [];
    const { type, lines } = message as Record<string, unknown>;
    return type === 'lines' && Array.isArray(lines) ? lines.filter(isWireLine) : [];
  } catch {
    return [];
  }
}

/**
 * Who may send lines: a Mac that transcribes the call, or did a moment ago
 * (its last words are on the way). Anyone can send on the topic, and put
 * words in anyone's mouth, so everyone else's are dropped.
 */
export class Transcribers {
  private now = new Set<string>();
  private stopped = new Map<string, number>();

  /** On every change to who's in the call and what they do. */
  update(room: Room) {
    const now = new Set<string>();
    for (const p of room.remoteParticipants.values()) {
      if (p.attributes[TRANSCRIBING_ATTRIBUTE]) now.add(p.identity);
    }
    for (const identity of this.now) if (!now.has(identity)) this.stopped.set(identity, Date.now());
    for (const identity of now) this.stopped.delete(identity);
    this.now = now;
  }

  trust(p: Participant) {
    if (p.attributes[TRANSCRIBING_ATTRIBUTE]) return true;
    const stopped = this.stopped.get(p.identity);
    return stopped !== undefined && Date.now() - stopped < LAST_WORDS_MS;
  }
}

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
    const key = `${l.by ?? by}/${l.id}`;
    const at = next.findIndex((x) => x.key === key);
    // Another Mac's line, relayed (history after a handover), only adds what
    // isn't here, or finishes a draft: it never changes or takes back a
    // final line.
    const relayed = (l.by ?? by) !== by;
    if (relayed && (!l.done || !l.text || (at >= 0 && next[at].done))) continue;
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
