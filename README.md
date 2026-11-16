# UHC-Link Raccourcis

Composant Windows 10/11 x64 pour les raccourcis du micro, du casque et du
push-to-talk. Il prend en charge le clavier et les boutons auxiliaires de souris,
même lorsqu'une autre application est au premier plan. La liaison locale utilise
WebSocket, avec compatibilité HTTP.

## Sources

- `src/`: programme Rust.
- `installer/windows.nsi`: installateur et désinstalleur NSIS.
- `resources/`: icône, manifeste et ressources Windows.
- `reproduce.py`: compilation reproductible de la version publiée.

## Compiler la version publiée

Version: **1.3.2**. Environnement requis: **Debian 13 x86_64**, directement ou
via WSL2. Les versions exactes des outils sont dans
`reproducibility/toolchain.json`, les dépendances sont fixées par `Cargo.lock`.

Installer [rustup](https://rustup.rs), puis les outils suivants:

```sh
sudo apt-get install --no-install-recommends python3 build-essential \
  nsis=3.11-1 nsis-common=3.11-1 \
  gcc-mingw-w64-base=14.2.0-19+27+b1 \
  gcc-mingw-w64-x86-64-posix=14.2.0-19+27+b1 \
  gcc-mingw-w64-x86-64-posix-runtime=14.2.0-19+27+b1 \
  binutils-mingw-w64-x86-64=2.44-3+12+b1 \
  mingw-w64-common=12.0.0-5 mingw-w64-x86-64-dev=12.0.0-5 \
  osslsigncode=2.9-2
rustup toolchain install 1.99.1 --profile minimal --component rustfmt --component clippy --target x86_64-pc-windows-gnu
python3 reproduce.py --verify-download
```

Le script compile le programme et les fichiers NSIS, puis rattache les signatures
publiques incluses après vérification des empreintes. Aucune clé privée n'est
nécessaire. L'option `--verify-download` compare aussi le résultat au fichier
actuellement téléchargeable. Ajouter `--offline` si les dépendances sont déjà
présentes localement.

Résultat: `dist/UHC-Link-Raccourcis.exe`.

SHA-256 attendu:

```text
cfae2dcba5674952eb2790a8d4a148a4b10c1fdde4fa7090210c983c4699c083
```

Les empreintes des trois exécutables figurent dans `reproducibility/release.json`.
Les signatures sont autosignées, elles ne constituent pas une certification
d'éditeur Windows.

## Modifier le programme

Modifier les fichiers de `src/` et de `installer/`. La reproduction exacte refuse
les changements qui modifient le binaire de référence. La commande suivante
compile uniquement le programme non signé, pas l'installateur distribué:

```sh
cargo build --release --target x86_64-pc-windows-gnu
```

## Licence

[MIT](LICENSE). Les mentions des dépendances sont dans
[THIRD-PARTY-LICENSES.txt](THIRD-PARTY-LICENSES.txt).
