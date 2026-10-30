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
* Icône dans la zone de notification. Sous Windows, la fenêtre peut s'y fermer
  et le programme peut se lancer à l'ouverture de session : un appui sur le
  bouton Scan rouvre alors la fenêtre et lance le scan.
* Détection du soulèvement de la souris : rien n'est ajouté tant qu'elle est en
  l'air. Une fois reposée sur une zone déjà scannée, sa position est retrouvée
  automatiquement, même décalée ou tournée d'environ 30°.
* Assemblage par recalage d'image (translation et rotation) sur la mosaïque déjà
  construite, avec fusion progressive des bords. Les deux capteurs laser de la
  souris donnent la prédiction de mouvement, rotation comprise, et prennent le
  relais sur les zones blanches.
* Correction automatique de l'éclairage et de la couleur, ou calibrage sur feuille blanche.
* Éditeur d'image avec annulation et rétablissement (Ctrl+Z, Ctrl+Y), zoom à la
  molette et déplacement au clic droit :
  * cadre de recadrage à poignées, avec une poignée ronde pour redresser l'image
    et une grille d'alignement, ainsi que le redressement automatique ;
  * gomme blanche, ou adaptative qui reconstitue le fond et le grain du papier
    autour de la zone effacée ;
  * rotation, miroir, luminosité, contraste, couleur, niveaux de gris ou noir et
    blanc adaptatif.
* Reconnaissance de texte (OCR) avec Tesseract, texte modifiable et copiable.
* Export en PNG, JPEG, TIFF, BMP, PDF image, PDF avec texte, TXT et Word (.docx).
* Ouverture d'une image existante pour l'OCR ou l'export.

## Installation

Les versions prêtes à l'emploi sont sur la page
[Releases](https://github.com/0ddityspace/mulot-scan/releases).

Pour l'OCR, installez aussi [Tesseract](https://tesseract-ocr.github.io/tessdoc/Installation.html)
avec les langues voulues. Mulot-scan fonctionne sans, mais la reconnaissance de
texte est alors indisponible.

### Windows 10 et 11

Lancez `mulot-scan-…-windows-x86_64-setup.exe`. L'installateur place le
programme dans `Program Files`, crée les raccourcis et s'enregistre dans
« Applications installées ». Une version portable sans installation est aussi
disponible en `.zip`.

Les versions publiées ne sont pas encore signées : au premier lancement, Windows
SmartScreen affiche « Windows a protégé votre ordinateur ». Cliquez sur
**Informations complémentaires**, puis **Exécuter quand même**.

Aucun pilote n'est nécessaire, la souris utilise le pilote vidéo USB intégré à
Windows. Le logiciel d'origine peut rester installé, mais fermez-le avant de
lancer Mulot-scan, car la caméra ne peut être ouverte que par un programme à la
fois. Pour Tesseract, l'installeur [UB Mannheim](https://github.com/UB-Mannheim/tesseract/wiki)
convient : Mulot-scan le trouve automatiquement dans `C:\Program Files\Tesseract-OCR`.

### Linux

Décompressez `mulot-scan-…-linux-x86_64.tar.gz` et lancez `mulot-scan`
(glibc 2.35 ou plus récente : Ubuntu 22.04, Debian 12, Fedora 36 et suivantes).

```sh
# Tesseract, par exemple :
sudo pacman -S tesseract tesseract-data-fra          # Arch
sudo apt install tesseract-ocr tesseract-ocr-fra     # Debian / Ubuntu
```

L'utilisateur doit pouvoir lire et écrire `/dev/video*`. C'est en général le cas
pour la session graphique, sinon ajoutez-le au groupe `video`.

### Depuis les sources

Avec Rust 1.95 ou plus récent (<https://rustup.rs>) :

```sh
cargo build --release
./target/release/mulot-scan
```

Sous Windows, il faut en plus les « Build Tools for Visual Studio », charge de
travail C++. Sous Linux, `make install` installe le programme, son icône et son
raccourci dans `~/.local`. La compilation croisée et la publication sont
décrites dans [docs/DEVELOPPEMENT.md](docs/DEVELOPPEMENT.md).

## Utilisation

1. Branchez la souris. L'indicateur en haut à droite passe au vert.
2. Posez-la sur le document et cliquez sur **Scanner**, ou appuyez sur son bouton Scan.
3. Déplacez-la lentement en passes qui se chevauchent. Si l'indicateur passe à
   l'orange, revenez sur une zone déjà scannée.
4. Arrêtez le scan, puis retouchez, lancez l'OCR et enregistrez.

Pour des fonds bien blancs, faites une fois **Réglages → Calibrer** avec la souris
immobile sur une feuille blanche vierge, sans impression au verso. Le texte ou
les taches éventuels sont ignorés, mais une feuille trop marquée est refusée.

Le résultat est tourné de 90° vers la gauche par défaut. Les réglages permettent
de changer cette orientation, de régler la sensibilité au soulèvement et de
désactiver la correction du miroir si le texte apparaît inversé.

Si le programme s'est arrêté brutalement pendant un scan, le pointeur peut rester
figé. Relancez Mulot-scan ou exécutez `mulot-scan reset`.

### Ligne de commande

```text
mulot-scan                         interface graphique
mulot-scan --tray                  démarre caché dans la zone de notification (Windows)
mulot-scan probe                   informations sur la souris
mulot-scan reset                   remet la souris en mode souris normal
mulot-scan --help                  toutes les options, dont celles de diagnostic
```

## Limites connues

* La version Windows n'a pas encore été testée sur une vraie machine Windows,
  ni le bouton Scan de la souris en conditions réelles.
* Les constantes des capteurs laser (échelle, position) ont été mesurées sur une
  seule souris. Si le suivi se dégrade sur un autre exemplaire, décochez
  « Utiliser les capteurs laser » dans les réglages.
* Après un soulèvement, la position est retrouvée par l'image seule : sur une
  zone peu marquée, la souris peut être recollée au mauvais endroit.
* De petits décalages restent visibles entre deux passes éloignées dans le temps,
  faute d'optimisation globale de la mosaïque.
* Le calibrage d'usine stocké dans la souris est lu mais pas interprété.
* La détection du soulèvement suppose que les octets 14 et 15 des données capteurs
  sont la qualité de surface. Leurs valeurs, entre 130 et 160 sur papier, le
  suggèrent, mais ce n'est pas confirmé. La chute de luminosité de l'image sert
  de détection de secours.

Le protocole de la souris est décrit dans [docs/PROTOCOLE.md](docs/PROTOCOLE.md).

## Licence

Au choix, sous licence [MIT](LICENSE-MIT) ou [Apache 2.0](LICENSE-APACHE).
