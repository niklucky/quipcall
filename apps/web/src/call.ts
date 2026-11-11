import { api } from './api';
import {
  Room,
  Track,
  type Participant,
  type ScreenShareCaptureOptions,
  type TrackPublishOptions,
  type VideoTrack,
} from 'livekit-client';

/** Mirrors `ScreenSharePreset` in crates/core/src/screen.rs. */
export interface ScreenSharePreset {
  fps: number;
  mode: 'text' | 'motion';
}

export const PRESETS: ScreenSharePreset[] = [
  { fps: 5, mode: 'text' },
  { fps: 15, mode: 'text' },
  { fps: 30, mode: 'motion' },
  { fps: 60, mode: 'motion' },
];

export const presetLabel = (p: ScreenSharePreset) => `${p.fps} fps · ${p.mode}`;

export function screenShareOptions(
  p: ScreenSharePreset,
): [ScreenShareCaptureOptions, TrackPublishOptions] {
  const text = p.mode === 'text';
  const maxBitrate = text ? (p.fps <= 5 ? 1_500_000 : 3_000_000) : p.fps <= 30 ? 4_000_000 : 6_000_000;
  return [
    {
      audio: false,
      contentHint: text ? 'detail' : 'motion',
      resolution: text
        ? { width: 2560, height: 1440, frameRate: p.fps }
        : { width: 1920, height: 1080, frameRate: p.fps },
    },
    {
      // H.264 for both modes: desktop receivers decode it in hardware, and
      // VP9 at this size is heavy for everyone (see docs/FOOTPRINT.md).
      videoCodec: 'h264',
      screenShareEncoding: { maxBitrate, maxFramerate: p.fps },
      degradationPreference: text ? 'maintain-resolution' : 'maintain-framerate',
    },
  ];
}

/** The backend derives identity and grants from the current session. */
export async function requestJoin(room: string) {
  return api<{url: string; token: string; identity: string}>('/join', {room});
}

export interface Tile {
  id: string;
  name: string;
  isLocal: boolean;
  isScreen: boolean;
  speaking: boolean;
  track?: VideoTrack;
}

export function collectTiles(room: Room): Tile[] {
  const people: Participant[] = [room.localParticipant, ...room.remoteParticipants.values()];
  const tiles: Tile[] = [];
  for (const p of people) {
    const base = {
      name: p.name || p.identity,
      isLocal: p === room.localParticipant,
      speaking: p.isSpeaking,
    };
    const videos = [...p.videoTrackPublications.values()].filter((pub) => pub.track && !pub.isMuted);
    if (videos.length === 0) tiles.push({ ...base, id: `p:${p.identity}`, isScreen: false });
    for (const pub of videos) {
      tiles.push({
        ...base,
        id: pub.trackSid,
        isScreen: pub.source === Track.Source.ScreenShare,
        track: pub.track as VideoTrack,
      });
    }
  }
  return tiles;
}

/** Set by the desktop app on whoever records the call. */
export const RECORDING_ATTRIBUTE = 'quipcall.recording';

/** Who records the call, if anyone does, and whether their screen is in it. */
export function recorder(room: Room): { name: string; screen: boolean } | null {
  for (const p of room.remoteParticipants.values()) {
    const kind = p.attributes[RECORDING_ATTRIBUTE];
    if (kind) return { name: p.name || p.identity, screen: kind === 'screen' };
  }
  return null;
}

/** Set by the desktop app whose Mac transcribes the call: the model's id. */
export const TRANSCRIBING_ATTRIBUTE = 'quipcall.transcribing';

/** Whose Mac transcribes the call, if anyone's. */
export function transcriber(room: Room): { identity: string; name: string } | null {
  for (const p of room.remoteParticipants.values()) {
    if (p.attributes[TRANSCRIBING_ATTRIBUTE]) return { identity: p.identity, name: p.name || p.identity };
  }
  return null;
}
