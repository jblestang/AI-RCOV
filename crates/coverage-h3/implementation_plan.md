# Architecture & Protocole d'Expérimentation : Export Couverture Radar vers H3 (Arch C Hiérarchique)

Ce document formalise l'architecture, la modélisation mathématique, le protocole de benchmark et le plan d'implémentation pour convertir une carte de couverture radar native (`.rhgt`) en cellules hexagonales H3 avec plancher d'altitude minimum discrétisé (`.jsonl`).

---

## 1. Contexte & Décisions Validées

1. **Source de données** : Fichier raster binaire natif `.rhgt` (`coverage-storage`), composé de pixels $u16$ représentant l'altitude minimale de détection (AMSL ou AGL) + métadonnées géoréférencées (projection locale AEQD, résolution métrique, origine, dimensions).
2. **Résolution H3** : **Résolution 7 par défaut** (surface ~5.16 km², arête ~1.22 km), entièrement configurable (ex: résolutions 6, 8, 9).
3. **Approche retenue** : **Architecture C (Reverse Pull Hiérarchique)** avec élagage top-down (Top-down spatial pruning).
   - Génération des cellules candidates à une résolution grossière (ex: Résolution 5).
   - Test d'intervisibilité rapide dans l'emprise raster.
   - Élagage immédiat des branches sans couverture.
   - Raffinement récursif jusqu'à la résolution cible (Résolution 7).
4. **Moteur H3** : **`h3o`** (100% Rust, zéro FFI C, SIMD, optimisé).
5. **Format d'export** : **JSONL (1 ligne par cellule visible)** avec rétention exclusive du **plancher minimum absolu** (la valeur la plus basse l'emporte) et ajout des **coordonnées de la frontière hexagonale (`boundary`)** précalculées en `[longitude, latitude]` WGS84 pour affichage direct.
6. **Périmètre du benchmark immédiat** : Focalisation sur **l'Architecture C**.

---

## 2. Spécification Technique & Algorithmique

### 2.1 Modèle Mathématique de l'Élagage Hiérarchique

Soit un radar positionné en $(\text{lat}_0, \text{lon}_0)$ avec une portée $R$ (ex: 400 km).
Soit la grille raster $G$ de dimensions $W \times H$ où chaque pixel $(c, r)$ a pour valeur $h(c, r) \in [0, 65534]$ ou $65535$ (`NO_DATA`).

```
                              Radar (Lat0, Lon0, R)
                                        │
                                        ▼
                             [Disque H3 à Res 5]
                           (h3o::grid_disk ~2000 hexs)
                                        │
                 ┌──────────────────────┴──────────────────────┐
                 ▼                                             ▼
          Hexagone A (Res 5)                            Hexagone B (Res 5)
        BBox Raster: [c0..c1, r0..r1]                 BBox Raster: [c0..c1, r0..r1]
       Tous pixels == NO_DATA ?                       Au moins 1 pixel != NO_DATA ?
                 │                                             │
                 ▼                                             ▼
          [ELAGAGE COMPLET]                             [DESCENTE Res 6]
      (aucun enfant évalué !)                       (7 enfants candidats)
                                                               │
                                                               ▼
                                                        [DESCENTE Res 7]
                                                    (Feuilles: Res Cible)
                                                               │
                                                               ▼
                                                    Calcul min_floor exact
                                                    + Polygon Boundary [lon, lat]
                                                               │
                                                               ▼
                                                      Stream Ligne JSONL
```

1. **Niveau Racine ($R_{\text{start}} = 5$)** :
   - Cellule centrale $C_0 = \text{lat\_lng\_to\_cell}(\text{lat}_0, \text{lon}_0, R_{\text{start}})$.
   - Candidats initiaux : $D = \text{grid\_disk}(C_0, k)$, avec $k = \lceil R / \text{diamètre\_hex}(R_{\text{start}}) \rceil$.
2. **Test d'intersection & d'éligibilité (Pruning)** :
   - Pour une cellule $C$ à la résolution $r$ :
     - Projeter les sommets de $C$ dans le repère métrique local via `LocalProjection::forward(lat, lon)` pour obtenir l'emprise raster $[c_{\min}..c_{\max}, r_{\min}..r_{\max}]$.
     - Si l'emprise est hors du raster ou si $\forall (c, r) \in \text{BBox}(C), h(c, r) = \text{NO\_DATA}$, **la cellule $C$ et tous ses descendants sont éliminés**.
3. **Évaluation aux Feuilles ($R_{\text{target}} = 7$)** :
   - Si $r = R_{\text{target}}$ et qu'au moins un pixel est valide :
     - Pour chaque pixel $(c, r)$ dans la boîte englobante de $C$ :
       - Vérifier l'appartenance géodésique : $\text{lat\_lng\_to\_cell}(\text{inverse}(c, r), R_{\text{target}}) == C$.
       - Mettre à jour : $\text{min\_floor}(C) = \min(\text{min\_floor}(C), h(c, r))$.
     - Si $\text{min\_floor}(C) \ne \text{NO\_DATA}$ :
       - Extraire la frontière : $\text{boundary} = C.\text{boundary}()$.
       - Émettre l'enregistrement JSONL.

### 2.2 Format de Sortie JSONL (Spécification RFC 7946)

Chaque ligne émise est un objet JSON compact conforme GeoJSON :

```json
{"h3":"871f9a269ffffff","res":7,"min_floor_m":124,"boundary":[[5.681234,43.310123],[5.688456,43.314567],[5.695123,43.310123],[5.695123,43.301234],[5.688456,43.296789],[5.681234,43.301234],[5.681234,43.310123]]}
```

*Notes techniques :*
- Les coordonnées sont sous la forme standard `[longitude, latitude]` en degrés décimaux.
- L'anneau polygonal est fermé (7 points, le dernier identique au premier).
- `min_floor_m` est un entier en mètres.

---

## 3. Architecture Logicielle & Découpage en Crates

Pour respecter l'isolation des dépendances de Radial (`coverage-core` restant `no_std` / sans dépendance externe lourde) :

### Nouvelle Crate : `crates/coverage-h3`
* **Rôle** : Transformation du format de persistance `.rhgt` en couverture discrétisée H3.
* **Dépendances** :
  - `h3o` (features `geo`, pure Rust)
  - `coverage-storage` (types `Metadata`, `RHGT_MAGIC`, `NO_DATA`)
  - `terrain-srtm` (pour `LocalProjection`)
  - `serde`, `serde_json`
  - `rayon` (parallélisation des branches racines)
* **API Publique** :
  ```rust
  pub struct H3ExportConfig {
      pub target_resolution: u8,   // Défaut : 7
      pub start_resolution: u8,    // Défaut : 5
      pub include_boundary: bool,  // Défaut : true
  }

  pub fn export_rhgt_to_h3_jsonl<W: std::io::Write>(
      metadata: &Metadata,
      heights: &[u16],
      config: &H3ExportConfig,
      writer: &mut W,
  ) -> Result<H3ExportStats, H3ExportError>;
  ```

---

## 4. Architectural Decision Record (ADR-0002)

Conformément à la règle `AGENTS.md`, une décision d'architecture sera formellement enregistrée dans `docs/adr/0002-h3-hierarchical-coverage-export.md` selon le modèle MADR.

* Extrait du contenu de l'ADR :
  - **Décision** : Implémenter l'extraction de couverture en cellules H3 via l'approche C hiérarchique avec la bibliothèque pure Rust `h3o` dans une crate dédiée `coverage-h3`.
  - **Pilotes de décision** : Temps de calcul pour 79M pixels (< 1 s visé), absence de dépendance C/FFI, streaming mémoire borné, élimination des zones masquées par élagage spatial.
  - **Options considérées** : Approche A (décodage tuiles PNG WMTS), Approche B (Forward push pixel par pixel), Approche C (Reverse pull hiérarchique).

---

## 5. Protocole d'Expérimentation & Benchmarking (TDD)

Conformément au skill `tdd-rust-perf` :

### Phase 1 : Tests Unitaires & Propriétés Fonctionnelles
Avant toute optimisation, créer le harnais de tests unitaires dans `crates/coverage-h3/tests/` :
1. **Test déterministe sur petit jeu de données** :
   - Utilisation d'un fichier `.rhgt` de test (ex: dataset `validate-e2e.sh` 787×296).
   - Validation que tous les hexagones générés contiennent bien au moins un pixel valide.
   - Validation que la valeur `min_floor_m` correspond exactement au $\min$ des pixels sous l'hexagone.
   - Validation de la géométrie de la frontière (polygone fermé à 7 sommets, coordonnées valides).
2. **Test des cas limites (Edge cases)** :
   - Radar entièrement dans le vide / NoData $\to$ 0 cellule émise.
   - Radar avec 1 seul pixel visible $\to$ 1 seule cellule H3 émise.
   - Résolutions cibles variables (6, 7, 8).

### Phase 2 : Benchmark Baseline Déterministe
Création de `benchmark/h3/main.rs` :
- **Jeu de données** : Matrice 8891×8891 (400 km / 90 m, 79 millions de pixels).
- **Environnement** : Compilation en `--release`, isolation des I/O via `std::io::sink()` ou buffer mémoire pré-alloué, `std::hint::black_box`.
- **Métriques relevées** :
  1. *Temps d'élagage racine (Res 5)* : temps pour éliminer les zones masquées.
  2. *Temps de descente & calcul des feuilles (Res 7)*.
  3. *Temps de sérialisation JSONL + boundary*.
  4. *Débit global* : Millions de pixels équivalents/s et Cellules H3/s.
  5. *Pic mémoire (RSS)* : impact du streaming vs stockage temporaire.

### Phase 3 : Optimisations Itératives Basées sur la Mesure
- Évaluation de l'algorithme d'inclusion point-dans-hexagone :
  - Variante 1 : Appel direct `h3o::LatLng::to_cell(res)`.
  - Variante 2 : Scanline raster / masque polygonale 2D local.
- Sérialisation JSONL zéro-allocation : utilisation d'un buffer d'écriture réutilisé (`itoa` / buffer direct) vs `serde_json`.

---

## 6. Plan d'Implémentation Étape par Étape

1. **Étape 1 : Création de l'ADR**
   - Rédiger et commiter `docs/adr/0002-h3-hierarchical-coverage-export.md`.
2. **Étape 2 : Initialisation de la crate `crates/coverage-h3`**
   - `Cargo.toml` avec dépendance `h3o = "0.8"` (ou version compatible stable), `coverage-storage`, `terrain-srtm`.
   - Définition des structures de données (`H3Record`, `H3ExportConfig`).
3. **Étape 3 : Implémentation du moteur hiérarchique**
   - Projection boîte englobante hexagone $\to$ indices raster.
   - Algorithme récursif de pruning (Res 5 $\to$ Res 7).
   - Calcul du plancher minimum et extraction des sommets WGS84.
4. **Étape 4 : Harnais de tests unitaires**
   - Validation d'exactitude sur le dataset de validation `validation-output/`.
5. **Étape 5 : Outil de Benchmark et CLI d'export**
   - Création du binaire de benchmark autonome `benchmark/h3/main.rs`.
   - Mesure de performance sur dataset 400 km / 90 m.
