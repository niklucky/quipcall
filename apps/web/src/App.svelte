<script lang="ts">
  import {
    Room,
    RoomEvent,
    Track,
    type RemoteTrack,
    type VideoCodec,
    type VideoTrack,
  } from 'livekit-client';
  import {
    PRESETS,
    collectTiles,
    presetLabel,
    recorder,
    transcriber,
    requestJoin,
    screenShareOptions,
    uniqueIdentity,
    type ScreenSharePreset,
    type Tile,
  } from './call';
  import {
    TRANSCRIPT_TOPIC,
    Transcribers,
    apply,
    askToStop,
    clock,
    linesIn,
    type Line,
  } from './transcript';

  // Test participants: `?room=spike&name=Bot&autojoin=1`, plus `codec=vp8`
  // (camera codec) and `mic=0` (join without a microphone).
  const params = new URLSearchParams(location.search);
  // H.264 by default: desktop receivers decode it in hardware and draw the
  // frames without a CPU copy (~40% less CPU than VP8 in our measurements).
  const codec = (params.get('codec') ?? 'h264') as VideoCodec;
  const withMic = params.get('mic') !== '0';
  const linkedRoom = params.get('room');
  let roomName = $state(linkedRoom ?? (import.meta.env.DEV ? 'spike' : ''));
  let name = $state(params.get('name') ?? localStorage.getItem('quipcall.name') ?? '');

  let room: Room | undefined;
  let status = $state<'idle' | 'joining' | 'connected'>('idle');
  let error = $state('');
  // LiveKit objects must not be deep-proxied by Svelte, hence $state.raw.
  let tiles = $state.raw<Tile[]>([]);
  let micOn = $state(false);
  let camOn = $state(false);
  let sharing = $state<ScreenSharePreset | null>(null);
  let recordedBy = $state<{ name: string; screen: boolean } | null>(null);
  let transcribedBy = $state<{ identity: string; name: string } | null>(null);
  let lines = $state<Line[]>([]);
  let transcriptOpen = $state(false);
  let transcriptEl = $state<HTMLElement | undefined>();
  let transcribers = new Transcribers();

  const refresh = () => {
    if (!room) return;
    tiles = collectTiles(room);
    micOn = room.localParticipant.isMicrophoneEnabled;
    camOn = room.localParticipant.isCameraEnabled;
    if (!room.localParticipant.isScreenShareEnabled) sharing = null;
    recordedBy = recorder(room);
    transcribers.update(room);
    const before = transcribedBy;
    transcribedBy = transcriber(room);
    // The transcript opens for everyone when it starts.
    if (transcribedBy && !before) transcriptOpen = true;
  };

  // Follows the newest line while you're at the bottom.
  $effect.pre(() => {
    void lines.length;
    const el = transcriptEl;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    if (atBottom) queueMicrotask(() => el.scrollTo({ top: el.scrollHeight }));
  });

  async function join() {
    error = '';
    status = 'joining';
    try {
      name = name.trim() || 'Guest';
      localStorage.setItem('quipcall.name', name);
      const { url, token } = await requestJoin(roomName.trim(), uniqueIdentity(name), name);
      const r = new Room({
        adaptiveStream: true,
        dynacast: true,
        publishDefaults: { videoCodec: codec },
      });
      transcribers = new Transcribers();
      // Lets headless tests inspect the call over the DevTools protocol.
      if (import.meta.env.DEV) Object.assign(window, { qcRoom: r });
      r.on(RoomEvent.TrackSubscribed, (track: RemoteTrack) => {
        if (track.kind === Track.Kind.Audio) document.body.appendChild(track.attach());
        refresh();
      })
        .on(RoomEvent.TrackUnsubscribed, (track: RemoteTrack) => {
          track.detach().forEach((el) => el.remove());
          refresh();
        })
        .on(RoomEvent.Disconnected, () => {
          room = undefined;
          tiles = [];
          lines = [];
          transcriptOpen = false;
          status = 'idle';
        })
        .on(RoomEvent.DataReceived, (payload, participant, _kind, topic) => {
          if (topic !== TRANSCRIPT_TOPIC || !participant) return;
          if (!transcribers.trust(participant)) {
            console.warn('transcript lines from someone who does not transcribe:', participant.identity);
            return;
          }
          const incoming = linesIn(payload);
          if (incoming.length) lines = apply(lines, participant.identity, incoming, r);
        });
      for (const ev of [
        RoomEvent.ParticipantConnected,
        RoomEvent.ParticipantDisconnected,
        RoomEvent.LocalTrackPublished,
        RoomEvent.LocalTrackUnpublished,
        RoomEvent.TrackMuted,
        RoomEvent.TrackUnmuted,
        RoomEvent.ActiveSpeakersChanged,
        RoomEvent.ParticipantAttributesChanged,
      ]) {
        r.on(ev, refresh);
      }
      await r.connect(url, token);
      room = r;
      status = 'connected';
      refresh();
      // Ask for devices after connecting so a slow permission prompt doesn't block joining.
      const devices = withMic
        ? r.localParticipant.enableCameraAndMicrophone()
        : r.localParticipant.setCameraEnabled(true);
      await devices.catch((e) => (error = `devices: ${e.message}`));
      refresh();
    } catch (e) {
      error = (e as Error).message;
      status = 'idle';
    }
  }

  /** Anyone can stop the transcript, for everyone. */
  async function stopTranscript() {
    if (room && transcribedBy) await askToStop(room, transcribedBy.identity);
  }

  async function leave() {
    await room?.disconnect();
  }

  async function toggleMic() {
    await room?.localParticipant.setMicrophoneEnabled(!micOn);
    refresh();
  }

  async function toggleCam() {
    await room?.localParticipant.setCameraEnabled(!camOn);
    refresh();
  }

  async function share(preset: ScreenSharePreset) {
    if (!room) return;
    try {
      if (sharing) await room.localParticipant.setScreenShareEnabled(false);
      const [capture, publish] = screenShareOptions(preset);
      await room.localParticipant.setScreenShareEnabled(true, capture, publish);
      sharing = preset;
    } catch (e) {
      error = `screen share: ${(e as Error).message}`;
    }
    refresh();
  }

  async function stopShare() {
    await room?.localParticipant.setScreenShareEnabled(false);
    sharing = null;
    refresh();
  }

  function attach(track: VideoTrack) {
    return (el: HTMLVideoElement) => {
      track.attach(el);
      return () => track.detach(el);
    };
  }

  // Screen shares go fullscreen on a double-click or the ⤢ button; Esc leaves.
  // iPhone Safari can only do that for the video element itself.
  function fullscreen(tile: HTMLElement) {
    if (document.fullscreenElement) {
      document.exitFullscreen();
      return;
    }
    const video = tile.querySelector('video') as
      | (HTMLVideoElement & { webkitEnterFullscreen?: () => void })
      | null;
    if (tile.requestFullscreen) tile.requestFullscreen().catch(() => {});
    else video?.webkitEnterFullscreen?.();
  }

  const cols = $derived(Math.max(1, Math.ceil(Math.sqrt(tiles.length))));

  if (params.get('autojoin') === '1') join();
</script>

<main>
  {#if status !== 'connected'}
    <form class="join" onsubmit={(e) => { e.preventDefault(); join(); }}>
      <h1>Quipcall</h1>
      <input bind:value={roomName} placeholder="Room" />
      <input bind:value={name} placeholder="Your name" />
      <button class="active" disabled={status === 'joining'}>
        {status === 'joining' ? 'Joining…' : 'Join'}
      </button>
      {#if error}<p class="error">{error}</p>{/if}
      {#if linkedRoom}
        <a href={`quipcall://join/${encodeURIComponent(linkedRoom)}`}>Open in the Quipcall app</a>
      {/if}
      <a class="dim" href="/download/">Get Quipcall for Mac</a>
    </form>
  {:else}
    <header>
      <span>In “{roomName}” as {name}</span>
      {#if recordedBy}
        <span class="rec">
          ● {recordedBy.name} is recording this call{recordedBy.screen ? ' and their screen' : ''}
        </span>
      {/if}
      {#if transcribedBy}
        <span class="rec">● {transcribedBy.name}'s Mac is transcribing this call</span>
      {/if}
      {#if error}<span class="error">{error}</span>{/if}
    </header>
    <div class="body">
    <section class="grid" style:grid-template-columns="repeat({cols}, 1fr)">
      {#each tiles as tile (tile.id)}
        <div
          class="tile"
          class:speaking={tile.speaking}
          role="figure"
          ondblclick={tile.isScreen ? (e) => fullscreen(e.currentTarget) : undefined}
        >
          {#if tile.track}
            <video
              {@attach attach(tile.track)}
              autoplay
              playsinline
              muted
              class:mirror={tile.isLocal && !tile.isScreen}
              class:contain={tile.isScreen}
            ></video>
          {/if}
          <span class="label">{tile.name}{tile.isLocal ? ' (you)' : ''}{tile.isScreen ? ' · screen' : ''}</span>
          {#if tile.isScreen && !tile.isLocal}
            <button
              class="expand"
              title="Fullscreen"
              aria-label="Fullscreen"
              onclick={(e) => fullscreen(e.currentTarget.parentElement!)}>⤢</button>
          {/if}
        </div>
      {/each}
    </section>
    {#if transcriptOpen}
      <aside class="transcript">
        <div class="transcript-head">
          <strong>Transcript</strong>
          <span class="muted">
            {transcribedBy ? `On ${transcribedBy.name}'s Mac` : 'Stopped'}
          </span>
          {#if transcribedBy}
            <button onclick={stopTranscript}>Stop for everyone</button>
          {/if}
          <button aria-label="Close" onclick={() => (transcriptOpen = false)}>×</button>
        </div>
        <div class="lines" bind:this={transcriptEl}>
          {#if lines.length === 0}
            <p class="muted empty">
              {transcribedBy ? 'What anyone says shows up here.' : 'Nobody transcribes this call. Start it from the Quipcall app for Mac.'}
            </p>
          {/if}
          {#each lines as line, i (line.key)}
            <div class="said" class:mine={line.mine}>
              {#if i === 0 || lines[i - 1].speaker !== line.speaker}
                <span class="who">{line.mine ? '' : `${line.name.split(' ')[0]} · `}{clock(line.atMs)}</span>
              {/if}
              <span class="bubble" class:draft={!line.done}>{line.text}</span>
            </div>
          {/each}
        </div>
      </aside>
    {/if}
    </div>
    <footer>
      <button class:active={micOn} onclick={toggleMic}>{micOn ? 'Mic on' : 'Mic off'}</button>
      <button class:active={camOn} onclick={toggleCam}>{camOn ? 'Camera on' : 'Camera off'}</button>
      <span class="muted">Share:</span>
      {#each PRESETS as preset (preset.fps)}
        <button class:active={sharing?.fps === preset.fps} onclick={() => share(preset)}>
          {presetLabel(preset)}
        </button>
      {/each}
      {#if sharing}<button onclick={stopShare}>Stop sharing</button>{/if}
      {#if transcribedBy || lines.length > 0}
        <button class:active={transcriptOpen} onclick={() => (transcriptOpen = !transcriptOpen)}>
          Transcript
        </button>
      {/if}
      <button class="danger" onclick={leave}>Leave</button>
    </footer>
  {/if}
</main>

<style>
  main { height: 100%; display: flex; flex-direction: column; }
  .join { margin: auto; display: flex; flex-direction: column; gap: 8px; width: 260px; }
  .join h1 { margin: 0 0 8px; font-size: 20px; }
  .join a { color: #8ab4ff; text-align: center; text-decoration: none; }
  .join a.dim { color: #9aa3b2; font-size: 12px; }
  header { display: flex; justify-content: space-between; padding: 8px 16px; border-bottom: 1px solid #262a33; }
  .body { flex: 1; display: flex; min-height: 0; }
  .grid { flex: 1; display: grid; gap: 8px; padding: 8px; min-height: 0; grid-auto-rows: 1fr; }
  .transcript { width: 320px; display: flex; flex-direction: column; border-left: 1px solid #262a33; min-height: 0; }
  .transcript-head { display: flex; align-items: center; gap: 8px; padding: 8px 12px; border-bottom: 1px solid #262a33; }
  .transcript-head .muted { margin-left: 0; flex: 1; min-width: 0; font-size: 12px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .transcript-head button { white-space: nowrap; }
  .transcript-head button { padding: 2px 8px; font-size: 12px; }
  .lines { flex: 1; overflow-y: auto; padding: 4px 12px 12px; }
  .empty { text-align: center; font-size: 12px; margin-top: 24px; }
  .said { display: flex; flex-direction: column; align-items: flex-start; margin-top: 3px; }
  .said.mine { align-items: flex-end; }
  .who { font-size: 11px; color: #676d78; margin: 8px 4px 3px; }
  .bubble { max-width: 86%; padding: 6px 10px; border-radius: 12px; background: #22242b; line-height: 18px; font-size: 13px; }
  .mine .bubble { background: rgb(79 140 255 / 28%); }
  .bubble.draft { color: #9ba1ac; }
  .tile { position: relative; background: #1b1e25; border-radius: 8px; overflow: hidden; border: 2px solid transparent; min-height: 0; }
  .tile.speaking { border-color: #3fb950; }
  video { width: 100%; height: 100%; object-fit: cover; display: block; }
  video.contain { object-fit: contain; }
  video.mirror { transform: scaleX(-1); }
  .label { position: absolute; left: 8px; bottom: 8px; padding: 2px 8px; border-radius: 4px; background: rgb(0 0 0 / 55%); }
  .expand { position: absolute; top: 8px; right: 8px; padding: 2px 8px; border: 0; border-radius: 4px; background: rgb(0 0 0 / 55%); color: #fff; font-size: 16px; }
  .tile:fullscreen { border: 0; border-radius: 0; background: #000; }
  .tile:fullscreen video { object-fit: contain; }
  footer { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; justify-content: center; padding: 12px; }
  .muted { color: #9aa3b2; margin-left: 12px; }
  .error { color: #ff7b72; }
  .rec { color: #ff6b61; font-weight: 600; }
  @media (max-width: 640px) {
    .grid { grid-template-columns: 1fr !important; }
    .body { flex-direction: column; }
    .transcript { width: auto; max-height: 40vh; border-left: 0; border-top: 1px solid #262a33; }
  }
</style>
