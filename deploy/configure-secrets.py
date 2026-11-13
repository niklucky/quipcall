#!/usr/bin/env python3
"""Merge optional auth settings and initialize missing LiveKit keys atomically.
Never evaluates env-file contents or prints credential values.
"""
import os
from pathlib import Path
import secrets
import shlex
import sys
import tempfile

AUTH = ('RESEND_API_KEY', 'QUIPCALL_MAIL_FROM', 'QUIPCALL_BOOTSTRAP_EMAIL')
LIVEKIT = ('LIVEKIT_API_KEY', 'LIVEKIT_API_SECRET', 'LIVEKIT_KEYS')


def read_env(path):
    values = {}
    if not path.exists():
        return values
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith('#'):
            continue
        key, sep, value = line.partition('=')
        if not sep or not key.replace('_', '').isalnum():
            raise ValueError('Invalid environment assignment')
        parts = shlex.split(value, comments=False)
        values[key] = ' '.join(parts)
    return values


def configure(path, auth=None):
    values = read_env(path)
    before = {key: values.get(key) for key in LIVEKIT}
    if auth is not None:
        incoming = read_env(auth)
        for key in AUTH:
            if incoming.get(key):
                values[key] = incoming[key]
    for key in AUTH:
        if not values.get(key):
            raise ValueError(f'Set {key} in the auth settings or secrets.env before deploying')
    # Recover a previously configured pair instead of rotating it accidentally.
    if values.get('LIVEKIT_KEYS'):
        key, sep, secret = values['LIVEKIT_KEYS'].partition(': ')
        if not sep or not key or not secret or '\n' in secret:
            raise ValueError('Expected one LiveKit key pair in LIVEKIT_KEYS')
        for name, value in zip(LIVEKIT[:2], (key, secret)):
            if values.get(name) and values[name] != value:
                raise ValueError('LiveKit settings disagree; reconcile them before deploying')
            values[name] = value
    values.setdefault('LIVEKIT_API_KEY', '')
    values.setdefault('LIVEKIT_API_SECRET', '')
    values['LIVEKIT_API_KEY'] = values['LIVEKIT_API_KEY'] or 'API' + secrets.token_hex(6)
    values['LIVEKIT_API_SECRET'] = values['LIVEKIT_API_SECRET'] or secrets.token_hex(24)
    values['LIVEKIT_KEYS'] = values['LIVEKIT_API_KEY'] + ': ' + values['LIVEKIT_API_SECRET']
    def quote(value):
        if '\n' in value or '\r' in value or '\0' in value:
            raise ValueError('Environment values must occupy one line')
        return '"' + value.replace('\\', '\\\\').replace('"', '\\"') + '"'
    content = ''.join(f'{key}={quote(value)}\n' for key, value in values.items())
    path.parent.mkdir(parents=True, exist_ok=True)
    if not path.exists() or path.read_text() != content:
        fd, temporary = tempfile.mkstemp(dir=path.parent, prefix='.secrets-')
        try:
            with os.fdopen(fd, 'w') as output:
                output.write(content)
            os.replace(temporary, path)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)
    os.chmod(path, 0o600)
    return any(values[key] != before[key] for key in LIVEKIT)


if __name__ == '__main__':
    try:
        changed = configure(Path(sys.argv[1]), Path(sys.argv[2]) if len(sys.argv) > 2 else None)
        if changed:
            print('livekit-keys-initialized')
    except (ValueError, OSError):
        # Parse errors can contain source text; never print secret-file contents.
        print('Cannot configure secrets: check required auth settings and consistent LiveKit keys.', file=sys.stderr)
        sys.exit(1)
