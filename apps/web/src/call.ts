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
  const maxBitrate = text ? (p.fps <= 5 ? 1_200_000 : 2_500_000) : p.fps <= 30 ? 4_000_000 : 6_000_000;
  return [
    {
      audio: false,
      contentHint: text ? 'detail' : 'motion',
      resolution: text
        ? { width: 2560, height: 1440, frameRate: p.fps }
        : { width: 1920, height: 1080, frameRate: p.fps },
    },
    {
      videoCodec: text ? 'vp9' : 'h264',
      screenShareEncoding: { maxBitrate, maxFramerate: p.fps },
      degradationPreference: text ? 'maintain-resolution' : 'maintain-framerate',
    },
  ];
}

/** Dev-only token endpoint (replaced by real auth in Stage 2). */
export async function devJoin(room: string, identity: string, name: string) {
  const q = new URLSearchParams({ room, identity, name });
  const res = await fetch(`/api/dev/token?${q}`);
  const body = await res.json();
  if (!res.ok) throw new Error(body.error ?? res.statusText);
  return body as { url: string; token: string };
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

export function uniqueIdentity(name: string) {
  const slug = name.toLowerCase().replace(/[^a-z0-9]+/g, '-');
  return `${slug}-${Math.random().toString(16).slice(2, 6)}`;
}
