#!/usr/bin/env python3
"""Single source of truth for every board the gate verifiers reserve.

Why this exists: the fresh-solve gates draw boards from finite pools, solved boards land in
the gitignored matrix/manifest.json, and hand-curated "disjoint" pools have collided three
times (last: verify_solve's final candidate 7s4h2d was also verify_trainer's asserted-absent
fixture, leaving the gates green only under an implicit run order). This registry makes the
overlap structurally impossible and makes the pools drain-proof:

- check_partitions() asserts the partitions below are pairwise DISJOINT (and every board
  well-formed + canonical), so putting a board in two roles fails loudly at gate setup.
- sweep(repo_root) removes consumable and absent-required boards from the matrix manifest
  (+ artifacts) at gate setup, so fresh-solve pools refill every run, absence fixtures
  self-heal, and the four gates pass in ANY order, repeatedly. Expected-cached fixtures are
  never touched.

Every verify_*.py calls prepare(REPO) before starting its server. Run this file directly to
check the partitions and print the table: python verifier_boards.py

Partition semantics (boards are bare canonical flops; gate keys derive via spot_key() as
{depth}bb_{board}_flop_{tree_id} with the tree id chosen by pot type):
- EXPECTED_CACHED: stable fixtures that must stay in the matrix (the curated batch + the
  viz static exports + the smoke board). Shared freely across gates AND across pot types
  (the same board may be cached under srp-v2, 3bp-v1, and 4bp-v1 keys - the cached role is
  never swept, so multi-pot reuse is safe; the Td9d6h smoke-v0 coexistence is the
  precedent). NEVER swept.
- SOLVE_CANDIDATES: verify_solve's fresh-solve pool (one consumed per run, swept back out).
- TRAINER_FRESH: verify_trainer's fresh-solve pool (likewise).
- TRAINER_ABSENT: the unsolved-spot 409 fixture; must never be in the manifest.
- REVIEW_PENDING: verify_review's pending-solve fixtures; must never be in the manifest.
"""

import json
import sys
from pathlib import Path

RANKS = "AKQJT98765432"
SUITS_DESC = "shdc"  # canonical suit order within a rank tie (matches gto-store)

# The bet trees the gates solve and expect: the SINGLE source for every key literal in the
# gate harness (phase1.toml's tree ids must match). Changing a tree costs one entry here,
# not a grep-and-replace - the same structural move as the board registry itself.
# Keys mirror gto_config::PotType wire strings; DEPTHS_BY_POT pins which depths exist per
# pot (4bp is 100bb only BY DECISION - 50bb 4-bet pots are jam-or-fold, out of scope), so
# the sweep never expects keys that were never buildable.
TREE_IDS = {"srp": "srp-v2", "3bp": "3bp-v1", "4bp": "4bp-v1"}
DEPTHS_BY_POT = {"srp": (50, 100), "3bp": (50, 100), "4bp": (100,)}

# Back-compat alias: the srp tree id (most call sites are srp-specific and predate pots).
TREE_ID = TREE_IDS["srp"]


def spot_key(depth, board, variant: str = "", pot: str = "srp") -> str:
    """The canonical gate-side spot key: {depth}bb_{board}_flop_{tree_id}{variant}.
    `pot` selects the tree id ("srp" default keeps every pre-pot call site unchanged)."""
    return f"{depth}bb_{board}_flop_{TREE_IDS[pot]}{variant}"

EXPECTED_CACHED = [
    "6c5d4h", "7h7d2c", "9c5d3h", "Jc9h8c", "Kh9h4h", "Ks7h2c",  # 100bb batch
    "8d6s3c", "AcKhQd", "Qs8s5h",                                # 50bb batch
    "Td9d6h",                                                    # smoke-v0 (Phase 0)
]
SOLVE_CANDIDATES = [
    "9s6h3d", "8h5c2s", "9d7s4c", "8c6d2h",
    "9h5s3c", "7d5h2c", "9c7h3s", "8s4d2c",
]
TRAINER_FRESH = ["Jh7c2s", "Qd7s2h", "Jc6d2s", "Td7h3c", "Qh6s2d"]
TRAINER_ABSENT = "7s4h2d"
REVIEW_PENDING = ["Th8c4d", "Jd8h5s", "Qc9d4s", "Th7s3h"]

# named cached fixtures (each must be a member of EXPECTED_CACHED; checked below)
SOLVE_CACHE_INTEROP = "AcKhQd"   # verify_solve 3d: Phase 1 batch interop
TRAINER_CACHED_50 = "8d6s3c"
TRAINER_CACHED_100 = "Ks7h2c"
TRAINER_CACHED_3BP = "9c5d3h"    # verify_trainer 3bp round (cached under the 3bp-v1 key)
REVIEW_CACHED_100 = ["Ks7h2c", "9c5d3h", "Jc9h8c"]
REVIEW_CACHED_50 = ["AcKhQd", "Qs8s5h", "8d6s3c"]

_PARTITIONS = {
    "expected-cached": EXPECTED_CACHED,
    "solve-consumable": SOLVE_CANDIDATES,
    "trainer-consumable": TRAINER_FRESH,
    "trainer-absent": [TRAINER_ABSENT],
    "review-pending-absent": REVIEW_PENDING,
}


def _assert_canonical(board: str, partition: str) -> None:
    assert len(board) == 6, f"{partition}: '{board}' is not a 3-card board"
    cards = [board[i:i + 2] for i in (0, 2, 4)]
    for c in cards:
        assert c[0] in RANKS and c[1] in SUITS_DESC, f"{partition}: illegal card '{c}' in '{board}'"
    assert len(set(cards)) == 3, f"{partition}: duplicate card in '{board}'"
    key = lambda c: (RANKS.index(c[0]), SUITS_DESC.index(c[1]))
    assert cards == sorted(cards, key=key), (
        f"{partition}: '{board}' is not canonical (rank desc, suit desc on ties)")


def check_partitions() -> None:
    """Loud setup guard: every board well-formed + canonical, partitions pairwise disjoint,
    named cached fixtures actually members of EXPECTED_CACHED, pot registry coherent."""
    assert set(TREE_IDS) == set(DEPTHS_BY_POT), (
        "TREE_IDS and DEPTHS_BY_POT must cover the same pot types")
    assert len(set(TREE_IDS.values())) == len(TREE_IDS), "tree ids must be distinct"
    for name, boards in _PARTITIONS.items():
        for b in boards:
            _assert_canonical(b, name)
    names = list(_PARTITIONS)
    for i, a in enumerate(names):
        for b in names[i + 1:]:
            overlap = set(_PARTITIONS[a]) & set(_PARTITIONS[b])
            assert not overlap, (
                f"board(s) {sorted(overlap)} are in BOTH '{a}' and '{b}': "
                f"one board, one role - fix verifier_boards.py")
    for fixture in [SOLVE_CACHE_INTEROP, TRAINER_CACHED_50, TRAINER_CACHED_100,
                    TRAINER_CACHED_3BP, *REVIEW_CACHED_100, *REVIEW_CACHED_50]:
        assert fixture in EXPECTED_CACHED, (
            f"named cached fixture '{fixture}' is not in EXPECTED_CACHED")


def sweep(repo_root) -> list:
    """Remove consumable and absent-required boards from the matrix (manifest entry +
    .bin.zst/.summary.json artifacts, every pot type at its allowed depths, shrunk variants
    included - a board consumed under ANY pot's key refills for all of them). Returns the
    removed keys. Never touches EXPECTED_CACHED boards, so the stable fixtures and the
    static viz exports stay reconciled."""
    matrix = Path(repo_root) / "matrix"
    manifest_path = matrix / "manifest.json"
    if not manifest_path.exists():
        return []
    manifest = json.loads(manifest_path.read_text())
    removed = []
    sweepable = [*SOLVE_CANDIDATES, *TRAINER_FRESH, TRAINER_ABSENT, *REVIEW_PENDING]
    for board in sweepable:
        for pot, depths in DEPTHS_BY_POT.items():
            for depth in depths:
                for variant in ("", "-shrunk"):
                    key = spot_key(depth, board, variant, pot)
                    if key in manifest["spots"]:
                        del manifest["spots"][key]
                        removed.append(key)
                    for suffix in (".bin.zst", ".summary.json"):
                        p = matrix / f"{key}{suffix}"
                        if p.exists():
                            p.unlink()
    if removed:
        manifest_path.write_text(json.dumps(manifest, indent=2))
    return removed


def prepare(repo_root, verbose: bool = True) -> None:
    """Call at the top of every gate verifier, before its server starts."""
    check_partitions()
    removed = sweep(repo_root)
    if verbose:
        if removed:
            print(f"[verifier_boards] swept {len(removed)} gate-run key(s) from the matrix: "
                  f"{', '.join(removed)}")
        else:
            print("[verifier_boards] partitions disjoint; no gate-run keys to sweep")


if __name__ == "__main__":
    check_partitions()
    print("partitions OK (pairwise disjoint, all boards canonical):")
    for name, boards in _PARTITIONS.items():
        print(f"  {name:<22} {', '.join(boards)}")
    sys.exit(0)
