import type { JoinOptions } from './media';

export type TestEntry = { name: string; options: JoinOptions };

/** Local development only; still uses normal session and room authorization. */
export function headlessEntry(query: URLSearchParams, hostname: string, development: boolean): TestEntry | undefined {
  if (!development || !['localhost', '127.0.0.1', '[::1]'].includes(hostname)
      || query.get('autojoin') !== '1' || !query.get('room') || query.has('desktop')) return;
  return { name: query.get('name')?.trim() || 'Test participant', options: {
    microphone: query.get('mic') !== '0', camera: query.get('camera') === '1',
  } };
}
