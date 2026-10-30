# Mulot-scan

<img src="assets/icon-256.png" alt="" width="96" align="right">

Logiciel libre pour Windows et Linux, compatible avec la souris scanner
IRIScan Mouse, aussi vendue sous le nom Dacuda Scanner Mouse (identifiant USB
`274f:c001`). Il remplace le logiciel Windows d'origine « Scanner Mouse ».

> Mulot-scan est un projet indépendant, sans lien avec I.R.I.S. ni Canon, ni
> approuvé par eux. IRIScan est une marque de ses propriétaires respectifs ; elle
> n'est citée ici que pour indiquer le matériel compatible.

## Fonctionnalités

* Détection automatique de la souris et reconnexion à chaud.
* Scan en direct : la mosaïque se construit pendant que vous déplacez la souris.
  Un indicateur signale un mouvement trop rapide ou une position perdue.
* Démarrage et arrêt avec le bouton Scan de la souris ou depuis l'interface.
* Détection du soulèvement de la souris : rien n'est ajouté tant qu'elle est en
  l'air. Une fois reposée sur une zone déjà scannée, sa position est retrouvée
  automatiquement, même décalée ou tournée d'environ 30°.
* Assemblage par recalage d'image (translation et rotation) sur la mosaïque déjà
  construite, avec fusion progressive des bords. Les deux capteurs laser de la
  souris donnent la prédiction de mouvement, rotation comprise, et prennent le
  relais sur les zones blanches.
* Correction automatique de l'éclairage et de la couleur, ou calibrage sur feuille blanche.
* Retouches : rotation, redressement automatique, recadrage, luminosité, contraste,
  couleur, niveaux de gris ou noir et blanc adaptatif.
* Reconnaissance de texte (OCR) avec Tesseract, texte modifiable et copiable.
* Export en PNG, JPEG, TIFF, BMP, PDF image, PDF avec texte, TXT et Word (.docx).
* Ouverture d'une image existante pour l'OCR ou l'export.

Les extensions de partage de l'ancien logiciel (Facebook, Twitter, Evernote,
Dropbox, Flickr, Google Translate) ne sont pas reprises.

## Installation

### Prérequis communs

* Rust 1.95 ou plus récent, via <https://rustup.rs>.
* Tesseract, facultatif mais nécessaire pour l'OCR.

### Linux

```sh
# Arch
sudo pacman -S tesseract tesseract-data-fra tesseract-data-eng
# Debian / Ubuntu
sudo apt install tesseract-ocr tesseract-ocr-fra tesseract-ocr-eng

cargo build --release
./target/release/mulot-scan
```

L'utilisateur doit pouvoir lire et écrire `/dev/video*`. C'est en général le cas
pour la session graphique, sinon ajoutez-le au groupe `video`.

### Windows

1. Installez les « Build Tools for Visual Studio », charge de travail C++.
2. Installez Tesseract, par exemple avec l'installeur UB Mannheim
   (<https://github.com/UB-Mannheim/tesseract/wiki>), en cochant les langues voulues.
   Le logiciel le trouve automatiquement dans `C:\Program Files\Tesseract-OCR`.
3. Compilez :

```bat
cargo build --release
target\release\mulot-scan.exe
```

Aucun pilote n'est nécessaire, la souris utilise le pilote vidéo USB intégré à
Windows. Le logiciel d'origine peut rester installé, mais fermez-le avant de
lancer mulot-scan, car la caméra ne peut être ouverte que par un programme à la fois.

## Générer les versions publiées

Le `Makefile` produit les archives dans `dist/` depuis Linux :

```sh
make linux-portable                 # Linux x86_64, compilé dans Docker, glibc ≥ 2.35
make windows ACCEPT_MSVC_LICENSE=1  # Windows 11 x86_64, exécutable autonome
make release ACCEPT_MSVC_LICENSE=1  # les deux + dist/SHA256SUMS
make help                           # toutes les cibles
```

* `make linux` compile directement sur la machine, plus vite. Le binaire exige
  alors la glibc locale et ne convient pas à la diffusion.
* La cible Windows est compilée de façon croisée et liée avec `lld-link`, du
  paquet `lld`. Les bibliothèques du SDK Windows sont téléchargées une fois
  auprès de Microsoft par l'outil `xwin`, d'où l'acceptation explicite de leur
  licence avec `ACCEPT_MSVC_LICENSE=1`.
* Variante sans SDK Microsoft : `make windows WINDOWS_ABI=gnu`, après avoir
  installé MinGW-w64 (Arch : `sudo pacman -S mingw-w64-gcc`).
* Sans rustup, le Makefile télécharge dans `build/toolchain` le compilateur Rust
  officiel de même version, car celui des distributions ne peut pas utiliser la
  bibliothèque standard Windows officielle.
* Pour le nettoyage, `make clean` supprime les archives et `make cleanall` les
  compilations, en gardant le compilateur et le SDK téléchargés. `make distclean`
  supprime tout ce qui a été généré ou téléchargé.

### Avec GitHub Actions

* `.github/workflows/ci.yml` compile et teste le projet sous Linux et Windows à
  chaque push et à chaque pull request.
* `.github/workflows/release.yml` produit les archives Linux et Windows 11. Il
  compile la version Linux dans le conteneur Debian 12 et la version Windows
  nativement, avec le runtime lié statiquement.

Pour publier une version, mettez à jour `version` dans `Cargo.toml`, puis poussez
le tag correspondant :

```sh
git tag v0.1.0
git push origin v0.1.0
```

La release GitHub est alors créée avec les deux archives et `SHA256SUMS`. Le
workflow peut aussi être lancé à la main depuis l'onglet Actions : les archives
sont alors disponibles comme artefacts, sans release.

## Utilisation

1. Branchez la souris. L'indicateur en haut à droite passe au vert.
2. Posez-la sur le document et cliquez sur **Scanner**, ou appuyez sur son bouton Scan.
3. Déplacez-la lentement en passes qui se chevauchent. Si l'indicateur passe à
   l'orange, revenez sur une zone déjà scannée.
4. Arrêtez le scan, puis retouchez, lancez l'OCR et enregistrez.

Sous Linux, `make install` installe le programme, son icône et son raccourci
dans `~/.local`. Ajoutez `PREFIX=/usr/local` avec sudo pour une installation globale.

Pour des fonds bien blancs, faites une fois **Réglages → Calibrer** avec la souris
immobile sur une feuille blanche vierge, sans impression au verso. Le texte ou les
taches éventuels sont ignorés, mais une feuille trop marquée est refusée. Le résultat est tourné de 90° vers la gauche
par défaut. Les réglages permettent de changer cette orientation, de régler la
sensibilité au soulèvement et de désactiver la correction du miroir si le texte
apparaît inversé.

### Ligne de commande

```text
mulot-scan                         interface graphique
mulot-scan --replay flux.mjpg      interface alimentée par un enregistrement
mulot-scan --record captures       interface ; chaque scan est aussi enregistré
                                   (captures/scan-001.mjpg, scan-002.mjpg…)
mulot-scan probe                   informations sur la souris
mulot-scan reset                   remet la souris en mode souris normal
mulot-scan record flux.mjpg 10     enregistre 10 s de flux brut
mulot-scan stitch flux.mjpg out.png  assemble un enregistrement hors ligne
```

Si le programme s'est arrêté brutalement pendant un scan, le pointeur peut rester
figé. Relancez mulot-scan ou exécutez `mulot-scan reset`.

## Organisation du code

| Module | Rôle |
|---|---|
| `src/device/` | Protocole matériel. `linux.rs` utilise V4L2 et l'ioctl UVC, `windows.rs` utilise Media Foundation et IKsControl. |
| `src/stitch/` | Assemblage. `stitcher.rs` gère le suivi de position, `register.rs` la corrélation de phase, `canvas.rs` la mosaïque en tuiles, `flat.rs` la correction d'éclairage, `laser.rs` le décodage des capteurs laser, `lift.rs` la détection du soulèvement et `output.rs` l'image finale. |
| `src/edit.rs` | Retouches |
| `src/ocr.rs` | Pilotage de Tesseract |
| `src/export.rs` | Formats de sortie |
| `src/app/` | Interface egui. `mod.rs` contient l'état et les actions, `panels.rs`, `views.rs` et `windows.rs` l'affichage. |
| `src/worker.rs` | Fils de traitement : détection de la souris, scan, calibrage |
| `src/cli.rs` | Sous-commandes `probe`, `record` et `stitch` |
| `examples/synth.rs` | Génère un enregistrement synthétique à partir d'une image, pour les tests |

Le protocole retrouvé est décrit dans [docs/PROTOCOLE.md](docs/PROTOCOLE.md).

## Limites connues

* Les constantes des capteurs laser (échelle, position) ont été mesurées sur une
  seule souris. Si le suivi se dégrade sur un autre exemplaire, décochez
  « Utiliser les capteurs laser » dans les réglages.
* Après un soulèvement, la position est retrouvée par l'image seule : sur une
  zone peu marquée, la souris peut être recollée au mauvais endroit.
* De petits décalages restent visibles entre deux passes éloignées dans le temps,
  faute d'optimisation globale de la mosaïque.
* La version Windows compile, mais elle n'a pas encore été testée sur une vraie
  machine Windows.
* Le calibrage d'usine stocké dans la souris est lu mais pas interprété.
* Le comportement du bouton Scan n'a pas encore été vérifié en conditions réelles.
* La détection du soulèvement suppose que les octets 14 et 15 des données capteurs
  sont la qualité de surface. Leurs valeurs, entre 130 et 160 sur papier, le
  suggèrent, mais ce n'est pas confirmé. La chute de luminosité de l'image sert
  de détection de secours.
