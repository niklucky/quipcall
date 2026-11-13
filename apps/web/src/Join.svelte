<script lang="ts">
  import { onMount } from 'svelte';
  import { api, type Entry, type Principal, type CallRoom } from './api';
  import type { TestEntry } from './test-entry';
  import { loadJoinEntry, type JoinFlow } from './join-flow';
  import { MediaPreview, type PreviewState, type JoinOptions } from './media';

  let { roomId, invitation, desktopId, me, callError, joining, flow, testEntry, identify, accepted, onjoin, onlogin, onhome }:
    { roomId: string; invitation: string; desktopId: string; me: Principal | null; callError: string; joining: boolean; flow: JoinFlow; testEntry?: TestEntry;
      identify: (name: string, current: () => boolean) => Promise<Principal>; accepted: (room: CallRoom) => void;
      onjoin: (id: string, name: string, options: JoinOptions, title: string) => Promise<void>; onlogin: () => void; onhome: () => void } = $props();
  let entry = $state<Entry | null>(null);
  let loading = $state(true);
  let error = $state('');
  let busy = $state(false);
  let done = $state(false);
  let desktopCode = $state('');
  let autojoined = false;
  $effect(() => {
    if (testEntry && !autojoined && !loading && allowed && !desktopId && !invitation) {
      autojoined = true; name = testEntry.name; void proceed('browser', testEntry.options);
    }
  });
  let openedMac = $state(false);
  let name = $state(localStorage.getItem('quipcall.name') ?? '');
  let identityId: string | undefined;
  let media = $state.raw<PreviewState>({ audioBusy: false, videoBusy: false, audioError: '', videoError: '' });
  let devices = $state<MediaDeviceInfo[]>([]);
  let audioId = $state('');
  let videoId = $state('');
  let level = $state(0);
  let video = $state<HTMLVideoElement>();
  const preview = new MediaPreview(navigator.mediaDevices, state => { media = state; });
  let request = 0;
  let alive = true;
  let attempt = 0;
  const working = $derived(busy || joining);
  const allowed = $derived(entry?.state === 'ready' || entry?.state === 'invite');
  const denied = $derived(entry && !allowed && entry.state !== 'signin');
  const title = $derived(desktopId ? 'Connect your Mac' : entry?.room?.name ?? (invitation ? 'Your invitation' : 'Join a call'));
  const descriptions: Record<string, [string, string]> = {
    expired: ['This invitation is no longer valid', 'It may have expired or been revoked. Ask the room owner for a new link.'],
    used: ['This invitation has been claimed', 'One person can use this link. If that was you, return in the same browser or sign in to the account you used. Otherwise, ask for a new invitation.'],
    removed: ['Your access was removed', 'Ask the room owner for a new invitation if you need to return.'],
    full: ['This 1-to-1 call already has its guest', 'Return with the same browser or account you used before, or ask the caller for a new call link.'],
    private: ['An invitation is needed', 'This room address only works for invited people. Sign in to your invited account or ask the owner for a guest invitation.'],
    unavailable: ['This room is unavailable', 'It may have been closed, or the link may be incomplete. Ask the person who invited you for a new link.'],
  };

  async function loadEntry() {
    const current = ++request;
    loading = true;
    try {
      const result = await loadJoinEntry({ desktopId, invitation, roomId }, body => api<Entry>('/entry', body));
      if (alive && current === request) entry = result;
    } catch (e) {
      if (alive && current === request) { entry = null; error = 'Could not check this link. Check your connection and try again.'; }
    } finally { if (alive && current === request) loading = false; }
  }
  $effect(() => { void desktopId; void roomId; void invitation; void me?.id; void loadEntry(); });
  $effect(() => {
    if (me && identityId !== me.id) { name = me.name; identityId = me.id; }
  });
  $effect(() => {
    if (video) { video.srcObject = media.video ? new MediaStream([media.video]) : null; }
  });
  $effect(() => {
    const track = media.audio;
    if (!track) { level = 0; return; }
    const context = new AudioContext();
    const source = context.createMediaStreamSource(new MediaStream([track]));
    const analyser = context.createAnalyser();
    analyser.fftSize = 256;
    source.connect(analyser); // Never connect the microphone to speakers.
    void context.resume().catch(() => {});
    const samples = new Uint8Array(analyser.fftSize);
    let frame: number;
    function tick() {
      analyser.getByteTimeDomainData(samples);
      level = Math.min(100, Math.sqrt(samples.reduce((sum, v) => sum + ((v - 128) / 128) ** 2, 0) / samples.length) * 450);
      frame = requestAnimationFrame(tick);
    }
    tick();
    return () => { cancelAnimationFrame(frame); source.disconnect(); void context.close(); };
  });
  async function enumerate() {
    try { const list = await navigator.mediaDevices?.enumerateDevices(); if (alive) devices = list ?? []; } catch { /* Manual controls still work. */ }
  }
  async function toggle(kind: 'audio' | 'video') {
    await preview.set(kind, !media[kind] && !media[`${kind}Busy`], kind === 'audio' ? audioId : videoId);
    await enumerate();
  }
  onMount(() => {
    void enumerate();
    const stop = () => { flow.cancel(); ++attempt; busy = false; preview.stop(); };
    window.addEventListener('pagehide', stop);
    navigator.mediaDevices?.addEventListener('devicechange', enumerate);
    return () => {
      alive = false; ++request; ++attempt; flow.cancel(); preview.dispose();
      window.removeEventListener('pagehide', stop);
      navigator.mediaDevices?.removeEventListener('devicechange', enumerate);
    };
  });
  async function proceed(destination: 'browser' | 'mac' | 'approve', testOptions?: JoinOptions) {
    if (working || loading || !allowed || !name.trim() || (destination === 'approve' && !/^[a-f0-9]{8}$/i.test(desktopCode.trim()))) return;
    const operation = ++attempt;
    const current = () => alive && operation === attempt;
    busy = true; error = '';
    const options: JoinOptions = testOptions ?? { microphone: !!media.audio, camera: !!media.video,
      audioDeviceId: media.audio?.getSettings().deviceId || audioId || undefined,
      videoDeviceId: media.video?.getSettings().deviceId || videoId || undefined };
    try {
      await flow.run({ destination, roomId, invitation, desktopId, desktopCode: desktopCode.trim(), name: name.trim(), title: entry?.room?.name ?? roomId, options }, {
        identify, accepted,
        redeem: token => api<CallRoom>('/invites/redeem', { token }),
        stopPreview: () => preview.stop(),
        approve: (id, code) => api('/auth/desktop/approve', { id, code }),
        approved: () => { done = true; },
        openMac: id => {
          openedMac = true;
          // Only the room address is sent; credentials use browser approval.
          location.href = `quipcall://open/${encodeURIComponent(id)}`;
        },
        join: onjoin,
      });
    } catch (e) {
      if (current()) { error = (e as Error).message || 'Could not join. Please try again.'; await loadEntry(); }
    } finally { if (current()) busy = false; }
  }
</script>

{#if done}
  <section class="result"><div class="success-mark">✓</div><h1>You’re ready</h1><p>Return to Quipcall on your Mac. You’ll continue as <strong>{name}</strong>.</p><button onclick={onhome}>Back to your rooms</button></section>
{:else if loading && !entry}
  <section class="result" aria-live="polite"><span class="eyebrow">GETTING READY</span><h1>Checking your link…</h1><p>One moment while we check room access.</p></section>
{:else if denied}
  <section class="result"><span class="eyebrow">ROOM ACCESS</span><h1>{descriptions[entry!.state][0]}</h1><p>{descriptions[entry!.state][1]}</p>
    <div class="result-actions">{#if ['private', 'used', 'full'].includes(entry!.state)}<button class="primary" onclick={onlogin}>Sign in to your account</button>{/if}<button onclick={() => { error = ''; void loadEntry(); }} disabled={loading}>Check again</button></div>
    <button class="text-button" onclick={onhome}>Back to Quipcall</button>
  </section>
{:else if entry?.state === 'signin'}
  <section class="result"><span class="eyebrow">MEMBER INVITATION</span><h1>You’re invited to {entry.room?.name}</h1><p>This invitation is for a specific account. Sign in with the email the room owner invited.</p>{#if me?.email}<p>You’re currently signed in as {me.email}.</p>{/if}<button class="primary" onclick={onlogin}>Sign in with the invited email</button><p class="fine">The invitation stays here while you sign in.</p></section>
{:else if !entry}
  <section class="result"><h1>Let’s try that again</h1><p role="alert">{error}</p><button class="primary" onclick={() => { error = ''; void loadEntry(); }}>Retry</button><button class="text-button" onclick={onhome}>Back to Quipcall</button></section>
{:else}
  <div class:desktop={!!desktopId} class="join-layout">
    {#if !desktopId}<div class="mobile-heading"><span class="eyebrow">{entry.room?.kind === 'private' ? 'PRIVATE ROOM' : entry.room?.kind === 'direct' ? '1-TO-1 CALL' : 'PUBLIC ROOM'}</span><h1>{title}</h1></div>{/if}
    {#if !desktopId}
      <section class="setup" aria-label="Camera and microphone preview">
        <div class="preview">
          {#if media.video}<video bind:this={video} autoplay muted playsinline aria-label="Your camera preview"></video>
          {:else}<div class="placeholder"><div class="avatar">{name.trim().slice(0, 1).toLocaleUpperCase() || 'Q'}</div><span>{media.videoBusy ? 'Waiting for camera permission…' : 'Camera is off'}</span></div>{/if}
          <span class="preview-label">Only you can see this</span>
        </div>
        <div class="media-controls">
          <button class:on={!!media.audio} aria-pressed={!!media.audio} disabled={working} onclick={() => toggle('audio')}>
            <svg viewBox="0 0 24 24" aria-hidden="true"><rect x="9" y="2" width="6" height="12" rx="3"/><path d="M5 10v2a7 7 0 0 0 14 0v-2M12 19v3M8 22h8"/>{#if !media.audio}<path d="m3 3 18 18"/>{/if}</svg>
            {media.audioBusy ? 'Cancel mic setup' : media.audio ? 'Mic on' : 'Mic off'}
          </button>
          <button class:on={!!media.video} aria-pressed={!!media.video} disabled={working} onclick={() => toggle('video')}>
            <svg viewBox="0 0 24 24" aria-hidden="true"><rect x="2" y="5" width="14" height="14" rx="3"/><path d="m16 9 6-4v14l-6-4"/>{#if !media.video}<path d="m3 3 18 18"/>{/if}</svg>
            {media.videoBusy ? 'Cancel camera setup' : media.video ? 'Camera on' : 'Camera off'}
          </button>
        </div>
        {#if media.audio}<div class="meter-row"><span>Mic level</span><div class="meter" role="meter" aria-label="Microphone level" aria-valuenow={Math.round(level)} aria-valuemin="0" aria-valuemax="100"><i style:width={`${level}%`}></i></div></div>{/if}
        {#if media.audioError}<p class="error" role="alert">{media.audioError}</p>{/if}
        {#if media.videoError}<p class="error" role="alert">{media.videoError}</p>{/if}
        <details><summary>Choose devices</summary><div class="device-fields">
          <label>Microphone<select bind:value={audioId} disabled={working || media.audioBusy} onchange={() => { if (media.audio) void preview.set('audio', true, audioId); }}><option value="">System default</option>{#each devices.filter(d => d.kind === 'audioinput' && d.deviceId) as d, i}<option value={d.deviceId}>{d.label || `Microphone ${i + 1}`}</option>{/each}</select></label>
          <label>Camera<select bind:value={videoId} disabled={working || media.videoBusy} onchange={() => { if (media.video) void preview.set('video', true, videoId); }}><option value="">System default</option>{#each devices.filter(d => d.kind === 'videoinput' && d.deviceId) as d, i}<option value={d.deviceId}>{d.label || `Camera ${i + 1}`}</option>{/each}</select></label>
        </div><p class="fine">Turn a device on to preview it and see its name.</p></details>
      </section>
    {/if}
    <section class="join-info">
      <div class:desktop-heading={!desktopId}><span class="eyebrow">{desktopId ? 'CONTINUE ON YOUR MAC' : entry.room?.kind === 'private' ? 'PRIVATE ROOM' : entry.room?.kind === 'direct' ? '1-TO-1 CALL' : 'PUBLIC ROOM'}</span>
      <h1>{title}</h1>
      <p class="intro">{desktopId ? 'Connect Quipcall to the identity you’re using here.' : invitation ? 'You’re invited. Get comfortable, then join when you’re ready.' : 'A moment to get ready. Join when it suits you.'}</p></div>
      <form onsubmit={(e) => { e.preventDefault(); void proceed(desktopId ? 'approve' : 'browser'); }}>
        <label>Your name<input bind:value={name} maxlength="64" autocomplete="nickname" placeholder="What should we call you?" required disabled={working} /></label>
        {#if me?.email}<p class="fine identity">Signed in as {me.email} <button type="button" class="text-button" disabled={working} onclick={onlogin}>Switch account</button></p>{/if}
        {#if desktopId}<label>Code shown on your Mac<input bind:value={desktopCode} autocomplete="off" autocapitalize="characters" spellcheck="false" maxlength="8" pattern="[a-fA-F0-9]{8}" placeholder="8-character code" required disabled={working} /></label><p class="fine">Open Quipcall on this Mac and type the code displayed there. Do not use a code sent by someone else. The Mac app uses its own microphone and camera settings.</p>{/if}
        <button class="primary join-button" disabled={working || loading || !name.trim() || (desktopId && !/^[a-f0-9]{8}$/i.test(desktopCode.trim())) || media.audioBusy || media.videoBusy}>{working ? (joining ? 'Joining…' : 'Getting ready…') : desktopId ? `Continue${name.trim() ? ` as ${name.trim()}` : ''}` : 'Join call'}<span aria-hidden="true">→</span></button>
      </form>
      {#if !desktopId}
        <p class="fine media-summary">{!media.audio && !media.video ? 'You’ll join in this browser with your mic and camera off.' : `${media.audio ? 'Microphone on' : 'Microphone off'} · ${media.video ? 'Camera on' : 'Camera off'}`}</p>
        <button class="mac-button" disabled={working || loading || !name.trim()} onclick={() => proceed('mac')}>Open in the Mac app <span aria-hidden="true">↗</span></button>
        {#if openedMac}<p class="fine" role="status">Approve the connection in the browser tab opened by Quipcall. If nothing opened, <a href="/download/">install or update the Mac app</a>, then try again. You can also join here.</p>{/if}
      {/if}
      {#if invitation && entry.inviteKind === 'guest'}<p class="access-note">This link is for one guest. Joining claims it for you; you can return until the owner removes your access.</p>{:else if me?.kind === 'guest' && entry.room?.kind === 'private'}<p class="access-note">Your access is remembered in this browser. Save this room’s address to come back.</p>{:else if entry.room?.kind === 'public'}<p class="access-note">Anyone with this room’s link can join.</p>{/if}
      {#if !me || me.kind === 'guest'}<p class="fine">Have an account? <button class="text-button" disabled={working} onclick={onlogin}>Sign in</button></p>{/if}
      {#if error || callError}<p class="error" role="alert">{error || callError}</p>{/if}
      <button class="text-button back" disabled={working} onclick={onhome}>Back to Quipcall</button>
    </section>
  </div>
{/if}

<style>
  h1 { font-size: clamp(26px, 3.4vw, 38px); letter-spacing: -.035em; line-height: 1.15; margin: 14px 0; overflow-wrap: anywhere; }
  p { line-height: 1.6; color: #a7acb9; } button { min-height: 44px; border-radius: 10px; font-weight: 550; } button:disabled { opacity: .45; cursor: default; }
  .join-layout { display: grid; grid-template-columns: minmax(0, 1.35fr) minmax(280px, 1fr); gap: clamp(28px, 5vw, 64px); align-items: center; padding: 32px 0; }
  .mobile-heading { display: none; }
  .join-layout.desktop { display: block; max-width: 440px; margin: 20px auto; }
  .eyebrow { font-size: 10px; font-weight: 650; letter-spacing: .16em; color: #8f9dba; }
  .intro { margin-bottom: 28px; font-size: 15px; } .join-info { min-width: 0; }
  label { display: flex; flex-direction: column; gap: 9px; color: #c9ceda; font-size: 13px; font-weight: 500; }
  input, select { width: 100%; min-width: 0; border: 1px solid #353944; background: #181b22; color: #f0f2f6; border-radius: 10px; padding: 12px 14px; font: inherit; font-size: 16px; }
  input:focus-visible, select:focus-visible, button:focus-visible, summary:focus-visible { outline: 2px solid #7fa6ff; outline-offset: 3px; }
  .primary { background: #4d83ff; color: white; padding: 12px 20px; } .join-button { display: flex; justify-content: space-between; gap: 12px; width: 100%; margin-top: 20px; font-size: 15px; overflow-wrap: anywhere; text-align: left; }
  .join-button span { font-size: 20px; } .mac-button { width: 100%; background: transparent; border: 1px solid #343945; margin-top: 4px; } .mac-button span { float: right; }
  .fine { font-size: 12px; line-height: 1.6; } .media-summary { margin: 10px 0 20px; } .identity { overflow-wrap: anywhere; }
  .text-button { display: inline; min-height: 0; padding: 3px 0; color: #9ab9ff; background: transparent; font: inherit; } .back { color: #888f9e; margin-top: 10px; font-size: 12px; }
  a { color: #9ab9ff; } .access-note { border-top: 1px solid #2a2e38; margin: 24px 0 16px; padding-top: 18px; font-size: 12px; }
  .preview { position: relative; aspect-ratio: 4 / 3; border-radius: 22px; overflow: hidden; background: radial-gradient(ellipse at 30% 10%, #2c3650, #1a202e 55%, #171b24); border: 1px solid #343c4e; }
  video { width: 100%; height: 100%; object-fit: cover; transform: scaleX(-1); } .placeholder { height: 100%; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 22px; color: #9aa3b6; font-size: 13px; }
  .avatar { width: 92px; height: 92px; display: grid; place-items: center; border-radius: 50%; background: #455879; color: #e7edfa; font-size: 36px; font-weight: 500; border: 1px solid #627496; }
  .preview-label { position: absolute; left: 18px; bottom: 16px; font-size: 11px; background: #111722a6; border-radius: 6px; padding: 6px 9px; color: #bac4d8; }
  .media-controls { display: flex; justify-content: center; gap: 12px; margin: 18px 0; } .media-controls button { display: flex; align-items: center; justify-content: center; gap: 10px; padding: 11px 18px; background: #252a35; border: 1px solid #363d4b; } .media-controls button.on { background: #24344e; border-color: #5477ab; }
  svg { width: 18px; height: 18px; stroke: currentColor; stroke-width: 1.7; fill: none; stroke-linecap: round; stroke-linejoin: round; flex-shrink: 0; }
  .meter-row { display: flex; align-items: center; justify-content: center; gap: 12px; font-size: 11px; color: #9da8bb; margin: 14px 0; } .meter { width: 130px; height: 5px; background: #30394b; border-radius: 3px; overflow: hidden; } .meter i { display: block; height: 100%; background: #86ba9c; transition: width .1s; }
  details { border-top: 1px solid #2a2f3b; padding-top: 16px; margin-top: 20px; } summary { cursor: pointer; color: #aab2c3; font-size: 12px; padding: 5px 0; } .device-fields { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; margin-top: 14px; } select { font-size: 13px; }
  .error { padding: 12px 14px; background: #46282d66; border: 1px solid #704047; border-radius: 10px; color: #ffc2c6; font-size: 13px; overflow-wrap: anywhere; }
  .result { max-width: 520px; margin: 9vh auto; text-align: center; padding: 20px; } .result p { margin: 20px 0; } .result-actions { display: flex; gap: 12px; justify-content: center; flex-wrap: wrap; margin: 24px 0; } .result > .text-button { display: block; margin: 20px auto; } .success-mark { display: inline-grid; place-items: center; width: 60px; height: 60px; background: #29483c; color: #a0dcb9; font-size: 28px; border-radius: 50%; }
  @media (max-width: 700px) { .preview-label { bottom: auto; top: 12px; } .mobile-heading { display: block; width: 100%; } .mobile-heading h1 { margin-bottom: 0; } .desktop-heading { display: none; } .join-layout { display: flex; flex-direction: column; gap: 20px; padding: 10px 0 28px; } .setup, .join-info { width: 100%; } .preview { aspect-ratio: 2 / 1; border-radius: 16px; } .avatar { width: 64px; height: 64px; font-size: 28px; } .placeholder { gap: 12px; } .intro { margin-bottom: 20px; } h1 { font-size: 28px; } .result { margin: 3vh auto; padding: 10px 0; } }
  @media (max-width: 380px) { .device-fields { grid-template-columns: 1fr; } .media-controls button { padding: 10px 12px; font-size: 12px; } }
</style>
