# Protocole de la souris scanner (USB 274f:c001)

Ce document décrit ce qui a été retrouvé par rétro-ingénierie des binaires
Windows d'origine (`DLL_OvtApi.dll`, `OVBaseIF.dll`, `SCCBCore.dll`,
`OVCamUVC_Ext.dll`, `DXCore1.dll`), puis vérifié sur une vraie souris sous Linux.

## Vue d'ensemble USB

La souris est un périphérique composite :

| Interface | Classe | Rôle |
|---|---|---|
| 0 | UVC Video Control | Contrôle caméra + unité d'extension n° 6 |
| 1 | UVC Video Streaming | Flux MJPEG 640×296 à 30 i/s |
| 2 | HID boot mouse | Souris classique |

Aucun pilote spécifique n'est nécessaire : `uvcvideo` (Linux) et `usbvideo.sys`
(Windows) suffisent.

## Unité d'extension

GUID `dd880f8a-1cba-4954-8a25-f7875967f0f7`, unité 6.

| Sélecteur | Taille | Usage |
|---|---|---|
| 1 | 20 octets, lecture | Informations. L'octet 0 est la variante de protocole (2 sur la souris testée). |
| 2 | 5 octets | Accès registres |
| 3 | 33 octets | Transferts EEPROM |

### Registres (sélecteur 2)

Format : `[commande, adresse_lo, adresse_hi, valeur_lo, valeur_hi]`.

* Lecture : `SET_CUR [0x80 | f, adr, 0, 0, 0]`, puis `GET_CUR`. L'adresse est
  renvoyée en écho et la valeur se trouve dans les octets 3 et 4.
* Écriture : `SET_CUR [0x00 | f, adr, 0, val_lo, val_hi]`.
* `f` vaut 0 pour le microcontrôleur de la souris (identifiant SCCB `0x88`).
  Les adresses sur 16 bits utilisent les commandes `0xa0` et `0x20`.

### Registres du microcontrôleur

| Registre | Rôle |
|---|---|
| `0xd8` | Mode. Écrire une commande puis relire jusqu'à l'accusé. |
| `0xbc` | État du bouton Scan (0 ou 1) |
| `0xe6` | Durée d'éclairage, en unités de 1/49,23 ms |
| `0xdc` | Position de lecture laser, de 0 à 128 |
| `0x2d` | Écrit à `0x60` avant le premier démarrage |
| `0xa1`–`0xa5` | Défi d'authentification |
| `0xb1`–`0xb5` | Réponse d'authentification |
| `0xb8`, `0xbd`, `0xbe` | Boîte aux lettres vers le capteur d'image (commande, adresse, donnée) |

Commandes du registre `0xd8` :

| Commande | Accusé | Effet |
|---|---|---|
| `0x00` | `0xff` | Désactive le mode souris |
| `0xe0` | `0x11` | Réactive le mode souris (envoyée deux fois) |
| `0xe1` | `0x99` | Passe en mode scan : éclairage et données capteurs |
| `0xed` / `0xec` | `0xff` | Étapes de l'authentification |

### Séquence de scan

1. Désactiver le mode souris.
2. Écrire `0x2d = 0x60` puis les réglages (exposition, gain, balance des blancs).
3. Démarrer le flux vidéo.
4. Écrire `0xd8 = 0xe1` et attendre `0x99`.
5. À l'arrêt, couper le flux puis réactiver le mode souris.

### EEPROM (sélecteur 3)

1. `SET [1, début_lo, début_hi, fin_lo, fin_hi, 0…]`, puis `GET`.
2. `SET [2]`, puis `SET [3]`.
3. Répéter `GET` : chaque réponse porte 32 octets de données à partir de l'octet 1.
4. `SET [5]` pour terminer.

Données connues :

* `0x3000` contient la longueur du calibrage d'usine, sur 2 octets.
* `0x3002` contient la somme de contrôle.
* `0x3006` contient le calibrage, une archive binaire boost::serialization d'environ 19 Ko.
* `0x7eb5` contient l'identifiant de la souris, sur 64 octets.

### Authentification

Un défi aléatoire `c` de 5 octets est écrit en `0xa1`–`0xa5`. Après les commandes
`0xed` puis `0xec`, la souris répond en `0xb1`–`0xb5` avec `T(c)`, où `T` est
implémentée par `security_transform` dans `src/device/protocol.rs`. La souris
testée répond correctement. Le mode scan fonctionne aussi sans cette étape.

## Données cachées dans les trames JPEG

Le firmware glisse ses données capteurs dans les en-têtes JPEG, ce qui rend les
trames brutes non décodables telles quelles :

* Les octets 5 à 8 du marqueur SOF0, normalement hauteur et largeur, portent le
  compteur de trames et trois octets de « bits hauts ». Il faut les remplacer par
  `01 28 02 80`, soit 296×640.
* Les 15 derniers coefficients de la deuxième table de quantification portent
  trois octets de « bits complémentaires » et douze octets de données. Il faut
  les remplacer par `0x19`.

Chaque octet de données est reconstitué par `unstuff()` dans
`src/device/frame.rs`. On obtient un enregistrement de 32 octets :

| Octets | Contenu |
|---|---|
| 0 | Compteur de trames |
| 2–13 | Six mots de 16 bits en gros-boutiste. Ce sont probablement les déplacements des deux capteurs laser. |
| 14–15 | Qualité de surface des deux capteurs |
| 16 | Octet supplémentaire |
| 17 | Indicateurs (bits 0 à 4) |
| 19 | Trame sautée |

Le logiciel d'origine détecte le soulèvement à partir des capteurs laser, avec
un seuil `liftoffSensitivityLaser` réglable, de 50 par défaut. mulot-scan applique
ce seuil aux octets 14 et 15.

Le sens exact des six mots n'a pas pu être établi sans enregistrement en
mouvement. L'assemblage actuel n'utilise que l'image.

## Optique

L'image de la caméra est vue en miroir. Le logiciel applique par défaut un
retournement vertical, puis tourne le résultat de 90° vers la gauche. Ces deux
réglages sont modifiables.
