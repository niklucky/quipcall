#!/bin/sh
# Runs on the VPS as root, from the bundle deploy.sh uploads. Safe to re-run.
#
# Layout: /opt/quipcall/{bin,web,download}, secrets.env (API keys, made here
# once), livekit.yaml; systemd units quipcall-livekit and quipcall-server;
# nginx site quipcall.com. Nothing else on the box is touched.
set -eu
STAGE=$(cd "$(dirname "$0")/.." && pwd)
ROOT=/opt/quipcall
DOMAIN=quipcall.com
SITE=/etc/nginx/sites-available/$DOMAIN.conf
LIVEKIT_VERSION=1.13.11

# Install $1 as $2 with mode $3 unless identical. Fails if nothing changed.
put() {
	cmp -s "$1" "$2" && return 1
	install -m "$3" "$1" "$2"
	echo "updated $2"
}

# Retry a check for up to 5 seconds.
eventually() {
	i=0
	until "$@" >/dev/null 2>&1; do
		i=$((i + 1))
		[ $i -ge 10 ] && return 1
		sleep 0.5
	done
}

id quipcall >/dev/null 2>&1 ||
	useradd --system --home-dir $ROOT --no-create-home --shell /usr/sbin/nologin quipcall
mkdir -p $ROOT/bin $ROOT/download /var/www/quipcall-acme

# LiveKit restarts drop every call in progress, so only when something changed.
restart_livekit=
if ! $ROOT/bin/livekit-server --version 2>/dev/null | grep -qx "livekit-server version $LIVEKIT_VERSION"; then
	tmp=$(mktemp -d)
	curl -fsSL "https://github.com/livekit/livekit/releases/download/v$LIVEKIT_VERSION/livekit_${LIVEKIT_VERSION}_linux_amd64.tar.gz" |
		tar -xz -C "$tmp" livekit-server
	install -m 755 "$tmp/livekit-server" $ROOT/bin/livekit-server
	rm -rf "$tmp"
	echo "installed LiveKit $LIVEKIT_VERSION"
	restart_livekit=1
fi

# API keys are generated here once and never leave the box.
if [ ! -f $ROOT/secrets.env ]; then
	key=API$(head -c 6 /dev/urandom | od -An -tx1 | tr -d ' \n')
	secret=$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')
	(
		umask 077
		printf 'LIVEKIT_API_KEY=%s\nLIVEKIT_API_SECRET=%s\nLIVEKIT_KEYS="%s: %s"\n' \
			"$key" "$secret" "$key" "$secret" >$ROOT/secrets.env
	)
	echo "generated API keys in $ROOT/secrets.env"
	restart_livekit=1
fi

# Auth deployment fails before replacing the running binary if email/bootstrap
# configuration is absent. Fill secrets.env from deploy/auth.env.example first.
for setting in RESEND_API_KEY QUIPCALL_MAIL_FROM QUIPCALL_BOOTSTRAP_EMAIL; do
    if ! grep -q "^${setting}=.\+" "$ROOT/secrets.env"; then
        echo "Set $setting in $ROOT/secrets.env before deploying auth" >&2
        exit 1
    fi
done

put "$STAGE/deploy/livekit.yaml" $ROOT/livekit.yaml 644 && restart_livekit=1
put "$STAGE/deploy/systemd/quipcall-livekit.service" /etc/systemd/system/quipcall-livekit.service 644 &&
	restart_livekit=1
put "$STAGE/deploy/systemd/quipcall-server.service" /etc/systemd/system/quipcall-server.service 644 || true
install -m 755 "$STAGE/quipcall-server" $ROOT/bin/quipcall-server.new
mv $ROOT/bin/quipcall-server.new $ROOT/bin/quipcall-server

# Web client: swap the whole directory at once.
rm -rf $ROOT/web.new $ROOT/web.old
cp -R "$STAGE/web" $ROOT/web.new
chmod -R u=rwX,go=rX $ROOT/web.new
[ -d $ROOT/web ] && mv $ROOT/web $ROOT/web.old
mv $ROOT/web.new $ROOT/web
rm -rf $ROOT/web.old
install -m 644 "$STAGE/deploy/download/index.html" "$STAGE/deploy/download/install.sh" $ROOT/download/

systemctl daemon-reload
systemctl enable --quiet quipcall-livekit quipcall-server
if [ -n "$restart_livekit" ]; then
	systemctl restart quipcall-livekit
	echo "restarted LiveKit"
else
	systemctl start quipcall-livekit
fi
systemctl restart quipcall-server

# First deploy: serve the ACME challenge and get a certificate. Renewal is
# certbot's timer; an existing deploy hook reloads nginx afterwards.
if [ ! -f /etc/letsencrypt/live/$DOMAIN/fullchain.pem ]; then
	install -m 644 "$STAGE/deploy/nginx/$DOMAIN.acme.conf" $SITE
	ln -sf $SITE /etc/nginx/sites-enabled/$DOMAIN.conf
	nginx -t && systemctl reload nginx
	certbot certonly --webroot -w /var/www/quipcall-acme -d $DOMAIN -d www.$DOMAIN \
		--key-type ecdsa --non-interactive --agree-tos
fi

# Other sites share this nginx: never leave it with a config it rejects.
if ! cmp -s "$STAGE/deploy/nginx/$DOMAIN.conf" $SITE; then
	[ -f $SITE ] && cp $SITE $SITE.bak
	install -m 644 "$STAGE/deploy/nginx/$DOMAIN.conf" $SITE
	ln -sf $SITE /etc/nginx/sites-enabled/$DOMAIN.conf
	if nginx -t; then
		systemctl reload nginx
		rm -f $SITE.bak
		echo "updated nginx site $DOMAIN"
	else
		echo "nginx rejected the new site; restored the previous one" >&2
		if [ -f $SITE.bak ]; then mv $SITE.bak $SITE; else rm -f $SITE /etc/nginx/sites-enabled/$DOMAIN.conf; fi
		exit 1
	fi
fi

for unit in quipcall-livekit quipcall-server; do
	if ! eventually systemctl is-active --quiet $unit; then
		journalctl -u $unit -n 30 --no-pager
		exit 1
	fi
done
eventually curl -fsS http://127.0.0.1:7880/ || {
	echo "LiveKit isn't answering" >&2
	exit 1
}
eventually curl -fsS --resolve $DOMAIN:443:127.0.0.1 https://$DOMAIN/api/health || {
	echo "https://$DOMAIN/api/health failed" >&2
	journalctl -u quipcall-server -n 30 --no-pager
	exit 1
}
echo "deployed: https://$DOMAIN"
