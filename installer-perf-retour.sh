#!/bin/sh
# Installe perf-retour (sans root) : Performance oubliée -> retour en Équilibré
# (secteur) ou Économie (batterie) après 5 min sans grosse charge CPU.
#   sh installer-perf-retour.sh            installer / mettre à jour
#   sh installer-perf-retour.sh --retirer  arrêter et désinstaller
set -e
SRC=$(dirname "$(readlink -f "$0")")
UNIT=$HOME/.config/systemd/user/perf-retour.service

if [ "$1" = "--retirer" ]; then
    systemctl --user disable --now perf-retour.service 2>/dev/null || true
    rm -f "$UNIT" "$HOME/.local/bin/perf-retour"
    systemctl --user daemon-reload
    echo "perf-retour retiré."
    exit 0
fi

cd "$SRC"
nice -n 19 cargo build --release -p perf-retour
install -m 755 target/release/perf-retour "$HOME/.local/bin/perf-retour"

cat > "$UNIT" <<'EOF'
[Unit]
Description=Quitte Performance après 5 min sans grosse charge CPU
After=graphical-session.target
PartOf=graphical-session.target

[Service]
ExecStart=%h/.local/bin/perf-retour
# Environment=PERF_RETOUR_DELAI=600 PERF_RETOUR_SEUIL=25
Restart=always
RestartSec=10
Nice=10

[Install]
WantedBy=graphical-session.target
EOF

systemctl --user daemon-reload
systemctl --user enable perf-retour.service
systemctl --user restart perf-retour.service
sleep 1
systemctl --user is-active perf-retour.service
"$HOME/.local/bin/perf-retour" etat
