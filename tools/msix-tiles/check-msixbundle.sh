#!/bin/bash
#
# Contrôle avant soumission : ouvre un .msixbundle et vérifie qu'aucune tuile
# n'est un aplat de couleur unique.
#
#   ./check-msixbundle.sh ~/Downloads/skimrr-msix/skimrr_0.3.1.0.msixbundle
#
# C'est exactement ce qui manquait : les paquets 0.3.0.0 et 0.3.1.0 sont partis
# avec une tuile large noire, et le défaut n'est revenu que par la certification
# Microsoft (10.1.1.11 On Device Tiles). Sort en code 1 si le paquet est mauvais,
# pour pouvoir être branché sur une CI.
set -euo pipefail

if [ $# -ne 1 ]; then
  echo "usage: $0 <paquet.msixbundle>" >&2
  exit 2
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Un .msixbundle est un zip de .msix, eux-mêmes des zips : deux niveaux à ouvrir.
unzip -o -q "$1" "*.msix" -d "$work"
unzip -o -q "$work"/*.msix "Assets/*" -d "$work"

python3 - "$work" <<'PY'
import sys, glob, os
from PIL import Image

bad = False
for path in sorted(glob.glob(os.path.join(sys.argv[1], "Assets", "*.png"))):
    image = Image.open(path).convert("RGBA")
    # Une tuile légitime a des centaines de couleurs ; un visuel de repli en a une.
    colours = len(image.getcolors(maxcolors=10**6))
    ok = colours > 1
    bad |= not ok
    verdict = "OK" if ok else "<-- VISUEL PAR DEFAUT"
    print(f'  {os.path.basename(path):24} {str(image.size):11} couleurs={colours:4} {verdict}')

print("\nRESULTAT:", "paquet OK" if not bad else "NE PAS SOUMETTRE")
sys.exit(1 if bad else 0)
PY
