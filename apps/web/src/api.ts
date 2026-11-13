export type Principal = { id: string; kind: 'user' | 'guest'; name: string; email: string | null; admin: boolean };
export type CallRoom = { id: string; name: string; kind: 'direct' | 'public' | 'private'; owner: string; guest: string | null; closed: boolean; expires: number | null };
export type Member = { id: string; name: string; kind: string; revoked: boolean };
export type Invite = { hash: string; kind: string; email: string | null; expires: number };
export type RoomDetail = { room: CallRoom; members?: Member[]; invites?: Invite[] };
export async function api<T>(path: string, body?: unknown, method?: string): Promise<T> {
  const response = await fetch(`/api${path}`, {
    method: method ?? (body === undefined ? 'GET' : 'POST'),
    credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json', 'X-Quipcall': '1' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!response.ok) {
    const value = await response.json().catch(() => ({}));
    throw new ApiError(value.error ?? `Request failed (${response.status})`, response.status);
  }
  return response.json();
}

export type Entry = {
  state: 'ready' | 'invite' | 'signin' | 'used' | 'expired' | 'removed' | 'full' | 'private' | 'unavailable';
  room?: Pick<CallRoom, 'id' | 'name' | 'kind'>;
  inviteKind?: 'guest' | 'member';
};
export class ApiError extends Error {
  constructor(message: string, public status: number) { super(message); }
}
