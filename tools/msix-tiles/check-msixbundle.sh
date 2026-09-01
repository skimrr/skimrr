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

# Delegates to the Python check, which the Windows CI job runs too — one definition
# of "is this tile a placeholder", not two that can drift apart.
exec python3 "$(dirname "$0")/check_bundle.py" "$1"
