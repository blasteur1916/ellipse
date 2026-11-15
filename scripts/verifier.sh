#!/usr/bin/env bash
# Vérifie Ellipse de bout en bout sur Linux ou macOS : installe les outils et les modèles comme au premier
# lancement, fabrique des fichiers d'essai (texte, PDF, photo, parole, vidéo), indexe tout (transcription
# comprise), puis cherche et échoue si le bon fichier n'est pas parmi les premiers résultats.
#   scripts/verifier.sh [dossier d'Ellipse] [dossier d'essai]
# À lancer depuis la racine du projet, après `cargo build --release -p ellipse-proto` et
# `cargo build --release -p ellipse-moteur --example installer`.
set -euo pipefail

RACINE="${1:-$HOME/ellipse-racine}"
ESSAI="${2:-$HOME/ellipse-essai}"
BIN="$(pwd)/target/release"
BASE="$RACINE/donnees/index.sqlite"

echo "== Installation (premier lancement)"
"$BIN/examples/installer" "$RACINE"
FFMPEG="$RACINE/outils/ffmpeg/ffmpeg"
"$FFMPEG" -hide_banner -version | head -1

echo "== Fichiers d'essai"
rm -rf "$ESSAI" && mkdir -p "$ESSAI"
# Un texte en français, cherché en anglais : la recherche passe d'une langue à l'autre.
cat > "$ESSAI/facture-edf-mars.txt" <<'TEXTE'
Facture d'électricité EDF, mars 2026.
Montant à payer : 84,20 euros avant le 12 avril 2026.
Consommation du mois : 312 kWh, abonnement inclus.
TEXTE
# Un PDF au vrai texte (écrit à la main : pas d'outil de plus à installer).
python3 - "$ESSAI/contrat-location.pdf" <<'PDF'
import sys
lignes = ["Residential lease agreement", "The tenant rents the apartment for 950 euros per month.",
          "The landlord keeps the deposit of two months of rent.", "This rental contract starts on 1 May 2026."]
flux = "BT /F1 16 Tf 60 760 Td 22 TL " + " ".join(f"({l}) Tj T*" for l in lignes) + " ET"
objets = [
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 842] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
    f"<< /Length {len(flux)} >>\nstream\n{flux}\nendstream",
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
]
sortie, positions = b"%PDF-1.4\n", []
for i, o in enumerate(objets, 1):
    positions.append(len(sortie))
    sortie += f"{i} 0 obj\n{o}\nendobj\n".encode()
xref = len(sortie)
sortie += f"xref\n0 {len(objets) + 1}\n0000000000 65535 f \n".encode()
sortie += "".join(f"{p:010d} 00000 n \n" for p in positions).encode()
sortie += f"trailer\n<< /Size {len(objets) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
open(sys.argv[1], "wb").write(sortie)
PDF
# Une photo de chien qui ne s'appelle pas « chien ».
curl -fsSL -o "$ESSAI/IMG_2045.jpg" https://raw.githubusercontent.com/pytorch/hub/master/images/dog.jpg
# Un message vocal (voix de synthèse du système).
PAROLE="The budget meeting is moved to Monday morning. Please bring the quarterly sales figures and the new price list."
if command -v espeak-ng > /dev/null; then
  espeak-ng -v en-us -s 150 -w "$ESSAI/brut.wav" "$PAROLE"
else
  say -o "$ESSAI/brut.aiff" "$PAROLE"
  "$FFMPEG" -hide_banner -loglevel error -i "$ESSAI/brut.aiff" "$ESSAI/brut.wav" && rm "$ESSAI/brut.aiff"
fi
"$FFMPEG" -hide_banner -loglevel error -i "$ESSAI/brut.wav" -ar 16000 -ac 1 "$ESSAI/message-vocal.m4a"
# Une vidéo : la photo du chien, avec la même voix, plus longue (la parole est cherchée par tranches).
"$FFMPEG" -hide_banner -loglevel error -i "$ESSAI/brut.wav" -af "apad=pad_dur=2" "$ESSAI/voix.wav"
"$FFMPEG" -hide_banner -loglevel error -loop 1 -i "$ESSAI/IMG_2045.jpg" -i "$ESSAI/voix.wav" -shortest \
  -vf "scale=640:-2,format=yuv420p" -r 2 "$ESSAI/clip-0007.mp4"
rm "$ESSAI/brut.wav" "$ESSAI/voix.wav"
ls -la "$ESSAI"

echo "== Indexation"
"$BIN/ellipse-proto" --racine "$RACINE" --base "$BASE" index "$ESSAI"

echo "== Recherches"
echecs=0
verifier() {
  local recherche="$1" attendu="$2"
  local sortie
  sortie="$("$BIN/ellipse-proto" --racine "$RACINE" --base "$BASE" cherche "$recherche" -n 3 2> /dev/null)"
  echo "$sortie"
  if echo "$sortie" | sed -n '/Par le contenu/,$p' | head -4 | grep -q "$attendu"; then
    echo "OK : « $recherche » → $attendu"
  else
    echo "ÉCHEC : « $recherche » n'a pas trouvé $attendu parmi les 3 premiers"
    echecs=$((echecs + 1))
  fi
}
verifier "electricity bill" "facture-edf-mars.txt"
verifier "rental contract" "contrat-location.pdf"
verifier "a dog" "IMG_2045.jpg"
verifier "budget meeting on monday" "message-vocal.m4a"
verifier "quarterly sales figures" "clip-0007.mp4"

if [ "$echecs" -gt 0 ]; then
  echo "== $echecs recherche(s) en échec"
  exit 1
fi
echo "== Tout est trouvé"
