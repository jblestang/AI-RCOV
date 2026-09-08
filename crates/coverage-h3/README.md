# coverage-h3

`coverage-h3` convertit les grilles de couverture radar haute performance du format natif `.rhgt` (`coverage-storage`) en cellules spatiales hexagonales [Uber H3](https://h3geo.org/) au format **JSONL**.

Chaque ligne émise représente une cellule H3 visible portant son **plancher minimum absolu d'altitude** (en mètres $u16$, la valeur la plus basse l'emporte) et sa **frontière polygonale fermée** WGS84 précalculée pour un affichage direct sur une carte (Leaflet, MapLibre, Deck.gl, Cesium, Canvas).

L'algorithme implémente **l'Architecture C (Reverse Pull Hiérarchique)** motorisée par [`h3o`](https://crates.io/crates/h3o) et [`rayon`](https://crates.io/crates/rayon), avec un élagage spatial top-down (Res 5 $\to$ Res 7) éliminant en amont les zones masquées par le relief ou hors de portée.

Pour plus de détails sur les choix d'architecture, voir [ADR-0002](../../docs/adr/0002-h3-hierarchical-coverage-export.md).

---

## Compilation

Compilez le binaire CLI d'exportation et de benchmark en mode Release :

```bash
cargo build --release -p coverage-h3 --bin h3_export
```

L'exécutable se trouvera sous :
- Linux / macOS : `./target/release/h3_export`
- Windows : `./target/release/h3_export.exe`

---

## Quickstart : Exporter un fichier `.rhgt` réel

### 1. Export vers un fichier `.jsonl`

```bash
# Export avec résolution H3 = 7 par défaut et pas d'altitude de 100 m
./target/release/h3_export.exe data/results/radar.rhgt export.jsonl --res 7 --bucket 100
```

### 2. Streaming direct vers la sortie standard (stdout)

```bash
# Aperçu des 5 premières lignes
./target/release/h3_export.exe data/results/radar.rhgt --res 7 | head -n 5
```

### 3. Exemple de sortie JSONL générée

Chaque ligne est un objet JSON compact conforme GeoJSON (RFC 7946) :

```json
{
  "h3": "873969a45ffffff",
  "res": 7,
  "min_floor_m": 124,
  "altitude_bucket_m": 100,
  "boundary": [
    [7.406154, 43.755901],
    [7.407807, 43.743373],
    [7.423803, 43.738815],
    [7.438150, 43.746784],
    [7.436501, 43.759314],
    [7.420501, 43.763872],
    [7.406154, 43.755901]
  ]
}
```

---

## Lancer les Benchmarks

Le binaire intègre un mode `--bench` autonome générant un volume de données synthétique représentatif de la production avec relief et ombres de masque.

### 1. Benchmark Standard (400 km / 90 m — 79 millions de pixels, Res 7)

```bash
./target/release/h3_export.exe --bench --res 7 --cell 90 --range 400000
```

Exemple de métriques obtenues :
- **Volume** : 8891 × 8891 (~79 Mpx, 55 Mpx visibles)
- **Temps médian** : ~1.55 s
- **Débit raster** : ~51 Mpx / s
- **Débit d'exportation** : ~57 000 cellules H3 / s (88 874 cellules exportées)

### 2. Benchmark Haute Résolution (200 km / 180 m — Res 8)

```bash
./target/release/h3_export.exe --bench --res 8 --cell 180 --range 200000
```

Exemple de métriques obtenues :
- **Volume** : 2225 × 2225 (5 Mpx)
- **Temps médian** : ~0.17 s
- **Débit d'exportation** : ~907 000 cellules H3 / s (156 492 cellules exportées)

---

## Options de la Ligne de Commande

| Option | Défaut | Description |
| :--- | :---: | :--- |
| `<input.rhgt>` | *(requis)* | Chemin du fichier binaire `.rhgt` en entrée. |
| `[output.jsonl]` | `stdout` | Chemin du fichier `.jsonl` en sortie. |
| `--bench` | `false` | Active le mode benchmark avec raster synthétique. |
| `--res <0-15>` | `7` | Résolution H3 cible (ex: 6, 7, 8, 9). |
| `--start-res <0-15>` | `5` | Résolution de départ pour l'élagage hiérarchique. |
| `--bucket <m>` | *(aucun)* | Quantifie l'altitude en tranches de $N$ mètres. |
| `--no-boundary` | `false` | N'exporte pas le tableau `boundary` (fichier plus léger). |
| `--cell <m>` | `90` | Taille de cellule en mètres (mode `--bench`). |
| `--range <m>` | `400000` | Portée radar en mètres (mode `--bench`). |

---

## Tests Automatisés

Pour lancer la suite de tests unitaires et d'intégration :

```bash
cargo test -p coverage-h3
```
