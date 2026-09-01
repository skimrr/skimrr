# Tuiles MSIX

Fabrique les visuels de tuiles du paquet Microsoft Store qui n'ont pas de source
dans `src-tauri/icons/`.

```sh
npm install     # une fois : puppeteer-core, qui pilote le Chrome déjà installé
npm run build   # réécrit src-tauri/gen/windows/Assets/Wide310x150Logo.png
```

## Pourquoi

`@choochmeque/tauri-windows-bundle` recopie `StoreLogo`, `Square44x44Logo` et
`Square150x150Logo` depuis `src-tauri/icons/`, mais il n'a aucune source pour la
tuile large 310x150 : il essaie de la fabriquer en collant l'icône carrée sur un
canevas, et cette étape a rendu un rectangle noir uni. Ce visuel par défaut a fait
échouer la certification du Store (10.1.1.11 On Device Tiles) sur
`skimrr_0.3.0.0.msixbundle`.

Les PNG produits sont versionnés dans `src-tauri/gen/windows/Assets/`, et le
bundler ne les régénère que si on lui passe `--regenerate-assets` — ce que le
workflow de build ne fait pas. Ne passez pas ce drapeau : il réécrirait la tuile
large avec le rectangle noir. Pour retoucher le visuel, éditez
`wide310x150.html` puis relancez `npm run build`.

## Contrôle avant soumission

```sh
./check-msixbundle.sh ~/Downloads/skimrr-msix/skimrr_0.3.1.0.msixbundle
```

Ouvre le paquet et refuse toute tuile réduite à une seule couleur. À lancer sur
l'artefact téléchargé depuis GitHub Actions, avant de l'envoyer au Partner Center.

`CHROME_PATH` permet de désigner un autre binaire Chrome que celui de macOS.
