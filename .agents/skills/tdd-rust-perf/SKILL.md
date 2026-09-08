---
name: tdd-rust-perf
description: "Optimisation de performances par TDD/Benchmark, agnostique du langage avec focus Rust (applicable à tout écosystème). Utilise ce skill dès qu'une optimisation de code, d'algorithme, de structures de données, d'allocations mémoire ou de requêtes est demandée. Le processus suit 3 phases strictes : analyse & hypothèse avec harnais de tests unitaires, benchmark de référence (baseline) validé avec l'utilisateur, puis optimisation itérative avec comparaison chiffrée, validation de non-régression et décision basée sur la mesure."
---

# TDD Performance (tdd-rust-perf)

Optimisation de performance rigoureuse en trois phases strictes basées sur la mesure. Ne pas brûler les étapes.

## Principe directeur : Pas d'optimisation sans mesure

Toute optimisation doit être prouvée par des chiffres concrets. On n'optimise jamais "au feeling" ou par intuition sans avoir mesuré la baseline avant et après.
À chaque intervention, respecter **KISS** : l'optimisation doit rester lisible, robuste et maintenable. On ne sacrifie jamais la clarté du code pour un gain marginal.

---

## Phase 1 — Profilage & Hypothèse

Avant d'écrire ou de modifier du code :

1. **Identifier la cible** : pointer le fichier, la fonction, la boucle critique ou le module concerné.
2. **Qualifier le goulot d'étranglement (Bottleneck)** :
   - **Complexité algorithmique** : passage de $O(N^2)$ à $O(N)$ ou $O(N \log N)$ (structures de hachage, btree, tri préalable, indexation).
   - **Allocations mémoire & copies excessives** (particulièrement critique en Rust / C++ / Go) :
     - Clones/copies inutiles au lieu de références / emprunts (`&[T]`, `&str`, `Cow<str>`).
     - Allocations répétées sur le tas (*heap*) dans des boucles chaudes (`Vec::new()`, `String` sans capacité réservée).
     - Absence de réutilisation de buffers préalloués (`clear()` plutôt que réallouer) ou `with_capacity`.
     - Objets volumineux passés par valeur ou boxed inutilement.
   - **Accès mémoire & Cache locality** : structure des données non contiguë en mémoire (AoS vs SoA, indirection de pointeurs, itérateurs non vectorisables).
   - **Contention de concurrence & verrous** : verrous (`Mutex`, `RwLock`) trop larges ou tenus trop longtemps, contention sur des atomiques, faux partage (*false sharing*).
   - **I/O & Sérialisation** : parsing redondant, roundtrips I/O non batchés, absence de bufferisation (`BufReader`, `BufWriter`).
3. **Formuler une hypothèse courte** :
   > "En remplaçant X par Y, on élimine Z (allocations / copies / passes / verrous), ce qui réduira le temps d'exécution / la mémoire de ~W%."
4. **Harnais de sécurité (Tests Unitaires Préalables)** :
   - Vérifier la couverture unitaire fonctionnelle de la cible (cas nominaux, cas limites / edge cases, collections vides, gestion d'erreurs).
   - Si la couverture est incomplète, **écrire les tests unitaires fonctionnels AVANT d'optimiser**.
   - **Présenter les tests unitaires ajoutés à l'utilisateur pour validation de leur pertinence**.
5. **Résumer en 3-5 lignes max**.

---

## Phase 2 — Benchmark Baseline (⛔ s'arrêter ici)

Écrire ou réutiliser un **test de benchmark reproductible et déterministe** :
- Utiliser un volume de données représentatif de la production (ex: $N = 1\,000$, $10\,000$, $100\,000$).
- Toujours exécuter en **mode optimisé** (ex: `cargo bench` ou `--release` en Rust, builds Release / AOT dans les autres langages).
- Isoler les tests des I/O externes imprévisibles (réseau, disque lent).
- Éviter l'élimination de code mort (*dead code elimination*) par le compilateur : en Rust, toujours forcer la consommation des valeurs et entrées avec `std::hint::black_box`.

### Structure d'un benchmark

#### Option A : Micro-benchmark avec `std::time::Instant` et `std::hint::black_box` (Rust autonome)
Idéal pour un benchmark rapide sans dépendance lourde :

```rust
use std::hint::black_box;
use std::time::Instant;

fn bench_operation() {
    let input_data = black_box(prepare_realistic_dataset(10_000));

    // Warmup : stabilisation du cache CPU et fréquence d'horloge
    for _ in 0..50 {
        black_box(target_operation(black_box(&input_data)));
    }

    // Mesure
    let iterations = 200;
    let start = Instant::now();
    for _ in 0..iterations {
        black_box(target_operation(black_box(&input_data)));
    }
    let elapsed = start.elapsed();
    let avg = elapsed / iterations;

    println!("Total: {:?} | Moyenne par op: {:?}", elapsed, avg);
}
```

#### Option B : Avec un framework dédié (`criterion` / `divan` en Rust, ou runner équivalent)
```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};

pub fn criterion_benchmark(c: &mut Criterion) {
    let data = prepare_realistic_dataset(10_000);

    c.bench_function("target_operation N=10000", |b| {
        b.iter(|| target_operation(black_box(&data)))
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
```

*(Dans d'autres langages : utiliser le benchmark runner standard comme `testing.B` en Go, `pytest-benchmark` en Python, `mitata`/`tinybench` en Node/TS).*

### Exécution de la baseline :
1. Lancer le benchmark sur le **code non optimisé actuel** (en build release / optimisé).
2. Récupérer les métriques initiales (**Baseline / Before** : temps moyen, médiane, débit, ou allocations).

⛔ **S'arrêter ici. Présenter les métriques de la Baseline à l'utilisateur et demander confirmation avant de passer à la Phase 3.**

> Message à envoyer : *"Voici la mesure de référence (Baseline) avant optimisation : [Tableau/Détail des temps et métriques]. Tu confirmes pour appliquer l'optimisation ?"*

---

## Phase 3 — Optimisation & Comparaison chiffrée

Seulement après validation de la baseline par l'utilisateur :

1. **Appliquer l'optimisation minimale** dans le code cible.
2. **Re-lancer le benchmark** dans les mêmes conditions exactes (même machine, même profil `--release`, même jeu de données) pour obtenir les métriques après optimisation (**After**).
3. **Vérifier la non-régression** en exécutant la suite de tests unitaires complète du projet (ex: `cargo test`).
4. **Calculer et présenter le tableau comparatif** :

| Opération / Cible | Volume (N) | Avant (Baseline) | Après (Optimisé) | Gain (%)     |
| ----------------- | ---------- | ---------------- | ---------------- | ------------ |
| `compute_los`     | 100 000    | 45.2 ms          | 12.1 ms          | **-73.2%** 🚀 |
| `parse_header`    | 10 000     | 1.85 µs          | 0.42 µs          | **-77.3%** 🚀 |

5. **Décision basée sur les chiffres et la mesure (Mesure > Intuition)** :
   - ✅ **Gain réel et significatif (> 3-5%)** : Conserver l'optimisation et proposer un message de commit conventionnel préfixé par `perf: {description du gain et métriques}`.
   - ⚠️ **Gain nul, négligeable (< 2-3%) ou complexification non rentable** : **Annuler immédiatement les changements** (`git checkout` / `git restore`). Ne jamais introduire de dette de complexité pour des micro-gains non prouvés.
   - 🔄 **Re-validation après rollback** : Si un changement est annulé, re-lancer les benchmarks et les tests unitaires pour confirmer le retour propre à l'état baseline stable.
   - Le processus est **strictement itératif et guidé par la mesure** : chaque optimisation doit faire ses preuves sur les chiffres.

---

## Anti-patterns à éviter

- ❌ Optimiser sans avoir mesuré la baseline avant.
- ❌ Benchmarker en mode Debug / non optimisé (toujours utiliser `--release` ou équivalent AOT/JIT optimisé).
- ❌ Oublier `black_box` : laisser le compilateur éliminer le calcul mort ou le pré-calculer à la compilation.
- ❌ Conserver du code obscur ou `unsafe` injustifié si le gain mesuré est nul ou négligeable.
- ❌ Sacrifier la robustesse ou la lisibilité pour un gain infinitésimal (< 1-2%).
- ❌ Créer des benchmarks non déterministes qui dépendent d'I/O instables, du réseau ou de la charge variable de l'OS.
- ❌ Oublier de lancer la suite de tests unitaires existante pour vérifier l'absence de régression fonctionnelle.
