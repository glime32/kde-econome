#!/bin/sh
# Installe econome (sans root) : Bluetooth coupé après 10 min sans appareil,
# écran à 60 Hz sur batterie, Conservation remise si tu restes branché plein.
#   sh installer-econome.sh            installer / mettre à jour
#   sh installer-econome.sh --retirer  arrêter et désinstaller
set -e
SRC=$(dirname "$(readlink -f "$0")")
UNIT=$HOME/.config/systemd/user/econome.service

if [ "$1" = "--retirer" ]; then
    systemctl --user disable --now econome.service 2>/dev/null || true
    rm -f "$UNIT" "$HOME/.local/bin/econome"
    systemctl --user daemon-reload
    echo "econome retiré."
    exit 0
fi

cd "$SRC"
nice -n 19 cargo build --release -p econome
install -m 755 target/release/econome "$HOME/.local/bin/econome"

cat > "$UNIT" <<'EOF'
[Unit]
Description=Économies auto : Bluetooth inutilisé, écran 60 Hz sur batterie, Conservation sur secteur
After=graphical-session.target bluetooth.target
PartOf=graphical-session.target

[Service]
ExecStart=%h/.local/bin/econome
Restart=always
RestartSec=10
Nice=10

[Install]
WantedBy=graphical-session.target
EOF

systemctl --user daemon-reload
systemctl --user enable econome.service
systemctl --user restart econome.service
sleep 1
systemctl --user is-active econome.service
"$HOME/.local/bin/econome" etat
