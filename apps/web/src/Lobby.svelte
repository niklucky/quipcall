<script lang="ts">
  import { onMount } from 'svelte';
  import Join from './Join.svelte';
  import { headlessEntry } from './test-entry';
  import { JoinFlow } from './join-flow';
  import type { JoinOptions } from './media';
  import { api, ApiError, type Principal, type CallRoom, type RoomDetail } from './api';
  let { onjoin, callError = '', joining = false }: { onjoin: (id: string, name: string, options: JoinOptions, title: string) => Promise<void>; callError?: string; joining?: boolean } = $props();
  const query = new URLSearchParams(location.search);
  const testEntry = headlessEntry(query, location.hostname, import.meta.env.DEV);
  const fragment = new URLSearchParams(location.hash.slice(1));
  // Keep pending tokens in this history entry, so a reload or sign-in does not
  // lose the invitation. They never enter a request URL or referrer.
  const pending = location.hash ? Object.fromEntries(fragment) : (history.state?.quipcall ?? {});
  let magic = $state(pending.magic ?? '');
  let accountInvite = $state(pending.accountInvite ?? '');
  let roomInvite = $state(pending.roomInvite ?? '');
  let desktop = $state(query.get('desktop') ?? '');
  let linkedRoom = $state(query.get('room') ?? '');
  let me = $state<Principal | null>(null);
  let rooms = $state<CallRoom[]>([]);
  let detail = $state<RoomDetail | null>(null);
  let email = $state('');
  let roomName = $state('');
  let editedRoomName = $state('');
  let kind = $state<'direct' | 'public' | 'private'>('private');
  let inviteEmail = $state('');
  let registrationEmail = $state('');
  let inviteUrl = $state('');
  let accountInviteUrl = $state('');
  let displayName = $state('');
  let message = $state('');
  let error = $state('');
  let busy = $state(false);
  let ready = $state(false);
  let showLogin = $state(false);
  const joinFlow = new JoinFlow();
  let entryVersion = $state(0);
  function cancelJoin() { joinFlow.cancel(); ++entryVersion; }
  const flow = $derived(!!(roomInvite || linkedRoom || desktop));
  const authScreen = $derived(!!(magic || accountInvite || showLogin));
  function remember() {
    history.replaceState({ ...history.state, quipcall: { magic, accountInvite, roomInvite } }, '', location.pathname + location.search);
  }
  remember();
  function setRoomUrl(id: string) {
    const url = new URL(location.href); url.hash = ''; url.searchParams.delete('manage');
    if (id) url.searchParams.set('room', id); else { url.searchParams.delete('room'); url.searchParams.delete('desktop'); }
    history.replaceState(history.state, '', url); remember();
  }
  function home() { cancelJoin(); linkedRoom = ''; roomInvite = ''; desktop = ''; magic = ''; accountInvite = ''; showLogin = false; error = ''; message = ''; setRoomUrl(''); void run(refresh); }
  function choose(id: string) { cancelJoin(); linkedRoom = id; roomInvite = ''; detail = null; setRoomUrl(id); }
  function accepted(room: CallRoom) { roomInvite = ''; linkedRoom = room.id; setRoomUrl(room.id); }
  async function refresh() {
    const previousId = me?.id;
    try { me = await api<Principal>('/auth/me'); }

    catch (e) { if (e instanceof ApiError && e.status === 401) me = null; else throw e; }
    if (previousId && me?.id !== previousId) cancelJoin();
    if (flow && showLogin && me?.kind === 'user' && me.id !== previousId) showLogin = false;
    rooms = me && !flow ? await api<CallRoom[]>('/rooms') : [];
    displayName = me?.name ?? '';
  }
  async function run(fn: () => Promise<void>) {
    if (busy) return;
    busy = true; error = ''; message = '';
    try { await fn(); } catch (e) { error = (e as Error).message; }
    finally { busy = false; }
  }
  async function identify(name: string, current: () => boolean): Promise<Principal> {
    let principal = me ?? await api<Principal>('/auth/guest', { name });
    if (!current()) return principal;
    if (principal.name !== name) principal = await api<Principal>('/auth/me', { name }, 'PATCH');
    if (current()) { me = principal; localStorage.setItem('quipcall.name', principal.name); }
    return principal;
  }
  async function inspect(id: string) { detail = await api<RoomDetail>(`/rooms/${id}`); editedRoomName = detail.room.name; inviteUrl = ''; }
  async function renameRoom() {
    if (!detail || detail.room.owner !== me?.id) return;
    const id = detail.room.id;
    await api(`/rooms/${id}`, { name: editedRoomName.trim() }, 'PATCH');
    await inspect(id); await refresh(); message = 'Room name saved.';
  }
  async function create() {
    const r = await api<CallRoom>('/rooms', { name: roomName.trim() || (kind === 'direct' ? 'Quick call' : 'New room'), kind });
    roomName = ''; await refresh(); await inspect(r.id);
  }
  async function invite(kind: 'member' | 'guest') {
    const result = await api<{url: string}>(`/rooms/${detail!.room.id}/invites`, { kind, email: inviteEmail });
    try { await inspect(detail!.room.id); } finally { inviteUrl = result.url; }
  }
  async function remove(id: string) {
    try { await api(`/rooms/${detail!.room.id}/members/${id}`, {}, 'DELETE'); }
    finally { await inspect(detail!.room.id); }
  }
  async function requestLink() {
    await api('/auth/request', { email, invite: accountInvite || undefined });
    message = 'If this email has an account or a valid invitation, a sign-in link is on its way. It expires in 15 minutes. After signing in, return to this tab.';
  }
  async function verify() { await api('/auth/verify', {token: magic}); magic = ''; accountInvite = ''; showLogin = false; remember(); await refresh(); message = 'Signed in. Return to your invitation or the tab where you connected your Mac.'; }
  onMount(() => {
    void run(async () => { try { await refresh(); const manage = query.get('manage'); if (me && manage) await inspect(manage); } finally { ready = true; } });
    const focus = () => { if (!busy && !joining) void refresh().catch(() => {}); };
    const hashChange = () => {
      cancelJoin();
      const next = new URLSearchParams(location.hash.slice(1));
      magic = next.get('magic') ?? magic; accountInvite = next.get('accountInvite') ?? accountInvite;
      roomInvite = next.get('roomInvite') ?? roomInvite; remember();
    };
    window.addEventListener('focus', focus); window.addEventListener('hashchange', hashChange);
    return () => { joinFlow.cancel(); window.removeEventListener('focus', focus); window.removeEventListener('hashchange', hashChange); };
  });
</script>

<div class="lobby" class:joining-flow={flow && !authScreen}>
  <header><button class="brand" onclick={home} disabled={joining}>Quipcall<span>Good company. One link away.</span></button><a href="/download/">Get the Mac app ↗</a></header>
  {#if !ready}<p>Loading…</p>
  {:else if flow && !authScreen}
    {#key entryVersion}
    <Join {testEntry} flow={joinFlow} roomId={linkedRoom} invitation={roomInvite} desktopId={desktop} {me} {callError} {joining} {identify} {accepted} {onjoin} onlogin={() => { cancelJoin(); showLogin = true; error = ''; message = ''; }} onhome={home} />
    {/key}
  {:else}
  {#if flow}<button class="back" onclick={() => { showLogin = false; magic = ''; accountInvite = ''; remember(); }}>← Back to your call</button>{/if}
  {#if me && !flow}<p class="account">{me.name} · {me.kind === 'guest' ? 'Guest on this browser' : me.email}
    <button disabled={busy} onclick={() => run(async () => { await api('/auth/logout', {}); me = null; rooms = []; detail = null; })}>Sign out</button>
  </p><form onsubmit={(e) => { e.preventDefault(); run(async () => { me = await api<Principal>('/auth/me', {name: displayName}, 'PATCH'); message = 'Display name saved.'; }); }}><input aria-label="Display name" bind:value={displayName} maxlength="64" required /><button disabled={busy}>Save name</button></form>{/if}
  {#if error || callError}<p role="alert" class="error">{error || callError}</p>{/if}
  {#if message}<p role="status" class="message">{message}</p>{/if}
  {#if magic}
    <section><h2>Finish signing in</h2><p>Confirm to use your single-use email link.</p><button disabled={busy} onclick={() => run(verify)}>Sign in to Quipcall</button></section>
  {:else if !me || me.kind === 'guest' || showLogin || accountInvite}
    <section>
      <h2>{accountInvite ? 'Accept your account invitation' : 'Sign in'}</h2>
      <p>Registration is by invitation. Guests can join calls without an account.</p>
      <form onsubmit={(e) => { e.preventDefault(); run(requestLink); }}>
        <input aria-label="Email" type="email" autocomplete="email" required bind:value={email} placeholder="you@example.com" />
        <button disabled={busy}>Email me a sign-in link</button>
      </form>
    </section>
  {/if}
  {#if !flow && me?.kind === 'user'}
    <section><h2>Start a call or create a room</h2>
      <form onsubmit={(e) => { e.preventDefault(); run(create); }}>
        <input aria-label="Room name" bind:value={roomName} maxlength="64" placeholder="Room name" />
        <select aria-label="Room type" bind:value={kind}><option value="direct">Guest 1-to-1</option><option value="public">Public room</option><option value="private">Private room</option></select>
        <button disabled={busy}>Create</button>
      </form>
      <p>{kind === 'direct' ? 'You and one guest. Unused links expire after 24 hours. Active calls stay open; keep the room to reuse its link.' : kind === 'public' ? 'Anyone with the link can join.' : 'Only invited members and guests can join.'}</p>
    </section>
  {/if}
  {#if !flow && rooms.length}
    <section><h2>Your rooms</h2><ul>{#each rooms as r (r.id)}<li><span>{r.name} <small>{r.kind}</small></span><button disabled={busy || joining} onclick={() => choose(r.id)}>Join</button><button disabled={busy} onclick={() => run(() => inspect(r.id))}>Details</button></li>{/each}</ul></section>
  {/if}
  {#if !flow && detail}
    <section>
      <h2>{detail.room.name}</h2><p>{detail.room.kind === 'private' ? 'Sharing this room address does not grant access. Use an invitation below.' : 'Share this link to invite someone to the call.'}</p>
      {#if detail.room.expires}
        <p>Available until {new Date(detail.room.expires * 1000).toLocaleString()} if unused. Active calls stay open.</p>
        {#if detail.room.owner === me?.id}
          <button disabled={busy} onclick={() => run(async () => { const id = detail!.room.id; await api(`/rooms/${id}/keep`, {}); await inspect(id); await refresh(); })}>Keep as room</button>
        {/if}
      {/if}
      <input aria-label="Room link" readonly value={`${location.origin}/?room=${detail.room.id}`} onclick={(e) => e.currentTarget.select()} />
      <a href={`quipcall://open/${detail.room.id}`}>Open in the Mac app</a>
      {#if detail.room.owner === me?.id}
        <form onsubmit={(e) => { e.preventDefault(); void run(renameRoom); }}>
          <input aria-label="Edit room name" bind:value={editedRoomName} maxlength="64" required disabled={busy} />
          <button disabled={busy || !editedRoomName.trim()}>Save room name</button>
        </form>
        {#if detail.room.kind === 'private'}
          <h3>Invite someone</h3>
          <button disabled={busy} onclick={() => run(() => invite('guest'))}>Create single-use guest link</button>
          <p>The guest can return until manually removed. Unclaimed links expire after 7 days.</p>
          <form onsubmit={(e) => { e.preventDefault(); run(() => invite('member')); }}><input aria-label="Member email" type="email" required bind:value={inviteEmail} placeholder="Member’s account email" /><button disabled={busy}>Create membership invitation</button></form>
        {/if}
        {#if inviteUrl}<label>Copy and share this invitation<input readonly value={inviteUrl} onclick={(e) => e.currentTarget.select()} /></label>{/if}
        {#if detail.invites?.length}<h3>Pending invitations</h3><ul>{#each detail.invites as i (i.hash)}<li><span>{i.email ?? 'Guest'} · expires {new Date(i.expires * 1000).toLocaleDateString()}</span><button disabled={busy} onclick={() => run(async () => { await api(`/rooms/${detail!.room.id}/invites/${i.hash}`, {}, 'DELETE'); await inspect(detail!.room.id); })}>Revoke link</button></li>{/each}</ul>{/if}
        {#if detail.members?.length}<h3>People with access</h3><ul>{#each detail.members as m (m.id)}<li><span>{m.name} · {m.kind}{m.revoked ? ' · removed' : ''}</span><button disabled={busy} onclick={() => run(() => remove(m.id))}>{m.revoked ? 'Retry disconnect' : 'Remove access'}</button></li>{/each}</ul>{/if}
        <button class="danger" disabled={busy} onclick={() => { if (confirm('Close this room and disconnect everyone?')) run(async () => { await api(`/rooms/${detail!.room.id}`, {}, 'DELETE'); detail = null; linkedRoom = ''; await refresh(); }); }}>Close room</button>
      {/if}
    </section>
  {/if}
  {#if !flow && me?.admin}
    <section><h2>Invite a new account</h2><form onsubmit={(e) => { e.preventDefault(); run(async () => { const result = await api<{url:string}>('/auth/invites', {email: registrationEmail}); accountInviteUrl = result.url; }); }}><input aria-label="New account email" type="email" required bind:value={registrationEmail} placeholder="Email address" /><button disabled={busy}>Create account invitation</button></form>
      {#if accountInviteUrl}<input aria-label="Invitation link" readonly value={accountInviteUrl} onclick={(e) => e.currentTarget.select()} />{/if}
    </section>
  {/if}
  {/if}
</div>
<style>
  .lobby { width: min(660px, 100%); margin: auto; padding: 24px; box-sizing: border-box; overflow-y: auto; }
  header { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 8px 0 28px; }
  .joining-flow { width: min(1080px, 100%); padding: 28px 40px; }
  .brand { background: none; padding: 0; text-align: left; font-size: 23px; font-weight: 650; letter-spacing: -.04em; }
  .brand span { display: block; color: #7e8799; font-size: 11px; font-weight: 400; letter-spacing: 0; margin-top: 5px; }
  .back { background: none; padding-left: 0; color: #9ab9ff; margin-bottom: 16px; }
  @media (max-width: 700px) { .lobby, .joining-flow { padding: 20px; } header { padding-bottom: 22px; } header a { font-size: 11px; margin: 0; } .brand span { display: none; } li { flex-wrap: wrap; } li span { flex-basis: 100%; } }
h2 { font-size: 17px; margin-top: 0; } h3 { font-size: 14px; }
  section { background: #1b1e25; border: 1px solid #30343e; border-radius: 12px; padding: 18px; margin: 16px 0; }
  form { display: flex; gap: 8px; flex-wrap: wrap; } input { min-width: 0; flex: 1; } input[readonly] { width: 100%; box-sizing: border-box; margin: 8px 0; }
  select { padding: 8px; background: #242832; color: inherit; border: 1px solid #454b58; border-radius: 6px; }
  p, small { color: #aeb5c4; font-size: 13px; line-height: 1.5; } a { color: #8ab4ff; margin: 8px; font-size: 13px; }
  .account { display: flex; justify-content: space-between; align-items: center; } ul { list-style: none; padding: 0; } li { display: flex; align-items: center; gap: 8px; padding: 8px 0; } li span { flex: 1; }
  .error { color: #ff9b94; } .message { color: #a9d9b8; } button:disabled { opacity: .5; cursor: wait; }
</style>
