import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('configure_secrets', Path(__file__).with_name('configure-secrets.py'))
config = importlib.util.module_from_spec(spec)
spec.loader.exec_module(config)
AUTH = 'RESEND_API_KEY=test-key\nQUIPCALL_MAIL_FROM="Quipcall <test@example.test>"\nQUIPCALL_BOOTSTRAP_EMAIL=admin@example.test\n'
PAIR = 'LIVEKIT_API_KEY=existing-key\nLIVEKIT_API_SECRET=existing-secret\n'


class SecretsTests(unittest.TestCase):
    def test_first_install_auth_only_and_keys_only_are_supported(self):
        for existing, supplied in [('', AUTH), (AUTH, None), (PAIR, AUTH), (PAIR + AUTH, None)]:
            with self.subTest(existing=bool(existing), supplied=bool(supplied)), tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / 'secrets.env'
                auth = Path(directory) / 'auth.env'
                if existing:
                    path.write_text(existing)
                if supplied:
                    auth.write_text(supplied)
                config.configure(path, auth if supplied else None)
                values = config.read_env(path)
                self.assertEqual(values['LIVEKIT_KEYS'], values['LIVEKIT_API_KEY'] + ': ' + values['LIVEKIT_API_SECRET'])
                if 'existing-key' in existing:
                    self.assertEqual(values['LIVEKIT_API_KEY'], 'existing-key')
                    self.assertEqual(values['LIVEKIT_API_SECRET'], 'existing-secret')
                self.assertEqual(values['QUIPCALL_MAIL_FROM'], 'Quipcall <test@example.test>')
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                before = path.read_bytes()
                self.assertFalse(config.configure(path, auth if supplied else None))
                self.assertEqual(path.read_bytes(), before)

    def test_recovers_existing_livekit_pair_without_rotating(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'secrets.env'
            path.write_text(AUTH + 'LIVEKIT_KEYS="old-key: old-secret"\n')
            config.configure(path)
            self.assertEqual(config.read_env(path)['LIVEKIT_API_SECRET'], 'old-secret')

    def test_missing_auth_or_conflicting_keys_does_not_modify_file(self):
        for initial in [PAIR, AUTH + PAIR + 'LIVEKIT_KEYS="different: key"\n']:
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / 'secrets.env'
                path.write_text(initial)
                with self.assertRaises(ValueError):
                    config.configure(path)
                self.assertEqual(path.read_text(), initial)

    def test_values_are_never_executed(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'secrets.env'
            marker = Path(directory) / 'executed'
            path.write_text(AUTH + f'EXTRA="$(touch {marker})"\n')
            config.configure(path)
            self.assertFalse(marker.exists())
            self.assertEqual(config.read_env(path)['EXTRA'], f'$(touch {marker})')


if __name__ == '__main__':
    unittest.main()
