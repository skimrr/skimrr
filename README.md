# Skimrr

Nettoyer une bibliothèque de photos avant tout autre tri. Skimrr fait deux choses :
repérer les **doublons** (exacts et quasi-identiques) et repérer les **photos floues**.
Rien d'autre.

**100 % local.** Aucune photo, aucune métadonnée ne quitte la machine. Pas de compte,
aucune télémétrie. L'application ne demande l'accès qu'au dossier que vous choisissez.
Seule l'activation de la licence contacte le réseau : une fois, puis environ une fois
par mois, et jamais avec la moindre donnée issue de vos photos.

## Fonctionnement

**Doublons.** Les fichiers de taille identique sont hachés en SHA-256 (en parallèle) :
ceux qui partagent une empreinte sont des copies exactes. En parallèle, chaque image
est décodée une fois pour en extraire une empreinte perceptuelle 64 bits (dHash), qui
rapproche les versions recadrées, recompressées ou retouchées. Le regroupement se fait
par union-find, avec un seuil de similarité ajustable en direct : le curseur ne
relance pas le scan, il ne fait que re-grouper.

**Flou.** Score de netteté par variance du Laplacien, sans IA ni GPU. L'image est
découpée en tuiles de 64 px et **le score est celui de la tuile la plus nette** : une
photo est réussie dès que *quelque chose* y est net. Moyenner sur toute l'image dit
l'inverse et signale à tort les portraits sur fond flou. Sur un bokeh de test, la
moyenne globale tombait à 16 % du score d'une photo nette, contre 47 % par tuiles. Un
percentile a été essayé puis écarté : un sujet occupant un dixième du cadre n'occupe
pas assez de tuiles pour le franchir, ce qui est précisément le cas à traiter.

Le score dépendant du contenu, le seuil n'est pas une constante : il est calibré sur la
médiane du dossier analysé (`médiane × 0,25`). L'onglet affiche une jauge de netteté
par vignette, relative à cette médiane, et garde à l'écran les photos situées **juste
au-dessus du seuil**, la difficulté d'un seuil étant de ne pas voir ce qu'il écarte de
peu.

**Guide.** Au tout premier lancement, un écran de trois points explique ce que Skimrr
cherche, ce qu'il ne fait pas quitter la machine, et comment les suppressions se
passent. Il ne revient plus ensuite, et reste accessible par le « ? » de la barre.

**Visionneuse.** Un bouton d'agrandissement apparaît au survol de chaque vignette et
ouvre la photo en plein écran, avec ses métadonnées et son score de netteté. Les
flèches du clavier parcourent le groupe (ou la sélection de photos floues), Échap
ferme, et depuis un groupe de doublons on peut désigner la version à conserver sans
revenir en arrière. Le fond de la visionneuse reste un neutre sombre dans les deux
thèmes : un entourage clair fausse la lecture du contraste, et c'est précisément ce
qu'on vient juger.

Le zoom se fait à la molette, au double-clic ou aux touches `+` / `-`, on se déplace
en glissant, et `0` réajuste. Le pourcentage affiché est celui des **pixels réels**,
pas de la taille ajustée : un clic dessus va exactement au 1:1, et au-delà de 100 %
l'image est interpolée : il n'y a plus rien à voir. Pour les raw et les HEIC, une
rendition pleine résolution est générée à la première ouverture plutôt qu'au scan,
car une bibliothèque entière coûterait bien plus cher que la poignée de photos qu'on
examine réellement. Le plafond de détail d'un raw reste la rendition qu'il embarque
(1920 px sur les ARW testés) : aller au-delà demanderait un dématriçage.

**Corbeille.** Aucune suppression définitive silencieuse. Les photos passent d'abord par
une grille de révision, on les voit toutes avant que quoi que ce soit ne bouge, et on
peut en retirer du lot d'un clic. Elles sont ensuite déplacées dans une corbeille locale
horodatée, accompagnée d'un manifeste qui permet la restauration à l'emplacement exact.
Le déplacement est tout-ou-rien : si un fichier échoue, le lot entier est remis en place.
Seul le vidage explicite de la corbeille supprime réellement.

## Développer

```sh
npm install
npm run tauri dev      # lance l'application
npm run build          # vérifie les types et compile le frontend
cargo test             # (dans src-tauri/) tests du backend
npm run tauri build    # génère le paquet de distribution
```

## Stack

Tauri 2 + React + TypeScript. Le backend Rust fait le scan, le hachage et l'analyse
d'image (`walkdir`, `rayon`, `sha2`, `image`, `kamadak-exif`) ; le frontend ne fait que
l'affichage. Interface en six langues (anglais, français, espagnol, allemand, japonais,
chinois simplifié) via react-i18next, avec les polices embarquées : Schibsted Grotesk,
Spline Sans Mono et Noto Sans JP/SC sous-ensemblées aux caractères de l'interface.

## Formats

JPEG, PNG, HEIC/HEIF, WebP, GIF, BMP, TIFF et les principaux formats raw (ARW, CR2,
CR3, NEF, ORF, RW2, RAF, PEF, DNG…) sont analysés entièrement : doublons exacts,
quasi-doublons et netteté.

Le HEIC, format par défaut des iPhone, passe par un décodeur HEVC en Rust pur
(crate `heic`) : aucune bibliothèque système à installer.

Les fichiers raw sont lus via la **rendition JPEG pleine taille** que leur conteneur
embarque, bien plus rapide qu'un dématriçage, et fidèle à ce que le photographe a vu.
Attention au piège : les données capteur contiennent régulièrement des séquences
`FF D8 FF`, donc un candidat n'est retenu que si son en-tête JPEG s'analyse
réellement ; choisir le plus gros bloc d'octets donne une image corrompue.

Ni la webview macOS ni WebView2 ne savent afficher un raw, et Chromium ne lit pas le
HEIC : ces fichiers reçoivent une vignette JPEG mise en cache (`app_cache_dir/previews`),
que l'interface affiche à la place de l'original.

La taille capteur d'un raw vit dans des tags propriétaires que Sony, notamment,
n'expose pas ; l'interface affiche donc le **format** (« ARW ») plutôt que des
dimensions trompeuses. Un raw est toujours préféré à son export JPEG au moment de
suggérer la version à conserver.

## Orientation

Un appareil enregistre la photo dans le sens du capteur et note dans un tag EXIF
comment le boîtier était tenu. Les navigateurs honorent ce tag pour les fichiers
qu'ils chargent eux-mêmes, mais les vignettes que Skimrr encode (raw, HEIC) sont
écrites sans EXIF : la rotation y est donc appliquée en dur. Le tag est également lu
pour les conteneurs raw, dont Sony, qui ne passent pas par le lecteur EXIF générique.

La rotation est appliquée **avant** le calcul de l'empreinte perceptuelle et du score
de netteté, sans quoi un portrait raw ne se regrouperait jamais avec son export
redressé : deux orientations donnent deux empreintes sans rapport.

## Limites connues

- **Date de prise de vue** : lue dans l'EXIF quand elle existe, sinon date de
  modification du fichier.
- **Grandes bibliothèques** : la comparaison des empreintes perceptuelles est
  quadratique ; au-delà de quelques dizaines de milliers de photos, un index dédié
  deviendra nécessaire.
- **Linux** : le rendu WebKitGTK n'a pas encore été testé.
