# Développement

## Organisation du code

| Module | Rôle |
|---|---|
| `src/device/` | Protocole matériel. `linux.rs` utilise V4L2 et l'ioctl UVC, `windows.rs` utilise Media Foundation et IKsControl. |
| `src/stitch/` | Assemblage. `stitcher.rs` gère le suivi de position, `register.rs` la corrélation de phase, `canvas.rs` la mosaïque en tuiles, `flat.rs` la correction d'éclairage, `laser.rs` le décodage des capteurs laser, `lift.rs` la détection du soulèvement et `output.rs` l'image finale. |
| `src/edit/` | Retouches. `erase.rs` contient la gomme. |
| `src/ocr.rs` | Pilotage de Tesseract |
| `src/export.rs` | Formats de sortie |
| `src/app/` | Interface egui. `mod.rs` contient l'état et les actions, `editor.rs` l'éditeur d'image, `tray.rs` la zone de notification, `panels.rs`, `views.rs` et `windows.rs` l'affichage. |
| `src/autostart.rs` | Lancement à l'ouverture de session (Windows) |
| `src/worker.rs` | Fils de traitement : détection de la souris, scan, calibrage |
| `src/cli.rs` | Sous-commandes `probe`, `record` et `stitch` |
| `examples/synth.rs` | Génère un enregistrement synthétique à partir d'une image |

Le protocole retrouvé est décrit dans [PROTOCOLE.md](PROTOCOLE.md).

## Outils de diagnostic

```text
mulot-scan --replay flux.mjpg        interface alimentée par un enregistrement
mulot-scan --record captures         interface ; chaque scan est aussi enregistré
                                     (captures/scan-001.mjpg, scan-002.mjpg…)
mulot-scan record flux.mjpg 10       enregistre 10 s de flux brut
mulot-scan stitch flux.mjpg out.png [trace.csv]
                                     assemble un enregistrement hors ligne ; la trace
                                     donne, image par image, la position estimée et
                                     les données des capteurs
cargo run --release --example synth -- page.png flux.mjpg
                                     enregistrement synthétique pour les tests
```

Les enregistrements peuvent contenir des documents personnels : `*.mjpg` est
ignoré par git.

`MULOT_SCAN_AUTOTEST=<dossier>` fait parcourir l'interface automatiquement
(scan, revue, recadrage, gomme, réglages) et enregistre une capture d'écran de
chaque étape, par exemple avec `--replay`.

## Générer les versions publiées

Le `Makefile` produit les archives dans `dist/` depuis Linux :

```sh
make linux-portable                 # Linux x86_64, compilé dans Docker, glibc ≥ 2.35
make windows ACCEPT_MSVC_LICENSE=1  # Windows 11 x86_64, exécutable autonome
make windows-installer ACCEPT_MSVC_LICENSE=1  # + installateur NSIS
make release ACCEPT_MSVC_LICENSE=1  # tout + dist/SHA256SUMS
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
* L'installateur est construit avec NSIS (`makensis`) ; s'il n'est pas installé,
  le Makefile le lance dans un conteneur Docker Debian.
* Avec un certificat de signature de code (`WINDOWS_CERT_PFX` pour le chemin du
  `.pfx`, `WINDOWS_CERT_PASSWORD`), le programme, l'installateur et le
  désinstalleur sont signés par `packaging/windows/sign.sh` (osslsigncode).
* `make clean` supprime les archives et `make cleanall` les compilations, en
  gardant le compilateur et le SDK téléchargés. `make distclean` supprime tout
  ce qui a été généré ou téléchargé.

## GitHub Actions

* `.github/workflows/ci.yml` vérifie le formatage, compile et teste le projet
  sous Linux et Windows à chaque push et à chaque pull request.
* `.github/workflows/release.yml` produit l'archive Linux, l'archive et
  l'installateur Windows. Il compile la version Linux dans le conteneur Debian 12
  et la version Windows nativement, avec le runtime lié statiquement. Si les
  secrets `WINDOWS_CERT_PFX_BASE64` et `WINDOWS_CERT_PASSWORD` sont définis dans
  le dépôt, les exécutables Windows sont signés.

Pour publier une version, mettez à jour `version` dans `Cargo.toml`, puis poussez
le tag correspondant :

```sh
git tag v0.2.0
git push origin v0.2.0
```

La release GitHub est alors créée avec les archives, l'installateur et
`SHA256SUMS`. Le workflow peut aussi être lancé à la main depuis l'onglet
Actions : les fichiers sont alors disponibles comme artefacts, sans release.
