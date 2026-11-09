#!/bin/sh
# Installe batterie-mode (mode de charge : conservation, normal, rapide), une règle sudo
# limitée à ces trois commandes pour econome et le widget, et un service qui remet
# Conservation à chaque démarrage.
#   pkexec sh installer-batterie-mode.sh            installer
#   pkexec sh installer-batterie-mode.sh --retirer  enlever
set -e
SRC=$(dirname "$(readlink -f "$0")")
BIN=/usr/local/bin/batterie-mode
REGLE=/etc/sudoers.d/batterie-mode
UNIT=/etc/systemd/system/conservation-mode.service

[ "$(id -u)" = 0 ] && [ -n "$PKEXEC_UID" ] || { echo "à lancer avec pkexec" >&2; exit 1; }

if [ "$1" = "--retirer" ]; then
    systemctl disable conservation-mode.service 2>/dev/null || true
    rm -f "$BIN" "$REGLE" "$UNIT"
    systemctl daemon-reload
    echo "batterie-mode retiré."
    exit 0
fi

U=$(id -nu "$PKEXEC_UID")
TMP=$(mktemp)
trap 'rm -f "$TMP"' EXIT
printf '%s ALL=(root) NOPASSWD: %s conservation, %s normal, %s rapide\n' "$U" "$BIN" "$BIN" "$BIN" > "$TMP"
visudo -cf "$TMP" >/dev/null || { echo "règle sudo invalide, rien n'a changé" >&2; exit 1; }

install -m 755 -o root -g root "$SRC/batterie-mode/batterie-mode" "$BIN"
install -m 440 -o root -g root "$TMP" "$REGLE"
cat > "$UNIT" <<EOF
[Unit]
Description=Batterie : mode Conservation à chaque démarrage

[Service]
Type=oneshot
ExecStart=$BIN conservation

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable conservation-mode.service
"$BIN" conservation && echo "batterie-mode installé, Conservation active." || echo "Installé, mais ce PC n'a pas de mode Conservation (voir le README)."
