# Source from the repository root in bash or zsh. Does not start services.
# Always allocate fresh state; never source dev.env for an isolated test stack.
export QUIPCALL_TEST_DIR="$(mktemp -d /tmp/quipcall-test.XXXXXX)"
export QUIPCALL_BIND=127.0.0.1:8180
export QUIPCALL_PUBLIC_URL=http://127.0.0.1:5273
export QUIPCALL_API=http://127.0.0.1:8180
export QUIPCALL_DATABASE_URL="sqlite://$QUIPCALL_TEST_DIR/quipcall.db"
export QUIPCALL_MAIL_DIR="$QUIPCALL_TEST_DIR/mail"
export QUIPCALL_BOOTSTRAP_EMAIL=admin@example.test
export QUIPCALL_DATA_DIR="$QUIPCALL_TEST_DIR/desktop"
export QUIPCALL_NO_MIC=1 QUIPCALL_FAKE_CAMERA=1 QUIPCALL_FAKE_SCREEN=1
export LIVEKIT_PUBLIC_URL=ws://127.0.0.1:7980
export LIVEKIT_API_URL=http://127.0.0.1:7980
export LIVEKIT_API_KEY=quipcall-dev
export LIVEKIT_API_SECRET=quipcall-dev-secret-0123456789abcdef
export QUIPCALL_LIVEKIT_CONFIG="$QUIPCALL_TEST_DIR/livekit.yaml"
cat > "$QUIPCALL_LIVEKIT_CONFIG" <<'YAML'
port: 7980
bind_addresses: [127.0.0.1]
rtc:
  tcp_port: 7981
  udp_port: 7982
  use_external_ip: false
keys:
  quipcall-dev: quipcall-dev-secret-0123456789abcdef
logging:
  level: info
YAML
