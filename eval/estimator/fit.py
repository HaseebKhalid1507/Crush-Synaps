#!/usr/bin/env python3
"""Fit crush-core's local token estimator against real count_tokens numbers.

Data: the stratified corpus slice from `sample_counts.py` plus every fixture,
each counted one request per text with both tokenizer families (the sqlite
cache, keyed by (model, sha256(text)), see lib/tokens.py). Features come from
the Rust estimator itself (`crush-eval-driver --features`), so the fit and the
runtime can't drift apart.

Model: tokens ~= w . features, fitted by non-negative weighted least squares
(weights 1/tokens, i.e. relative error) on a seeded 80% split; the other 20%
is held out. Non-negative weights keep the estimate monotone: adding text never
lowers it. Writes crates/crush-core/src/estimate_coef.rs and an aggregate
report to eval/out/estimator.json (no per-sample data).

  --check   don't rewrite the coefficients; just score the current ones
"""
from __future__ import annotations

import argparse
import hashlib
import json
import sqlite3
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from common import CORPUS_DIR, OUT, REPO, read_jsonl, sha  # noqa: E402
from run_crush import DRIVER, load_samples  # noqa: E402

MODELS = {"V5": "claude-opus-5-5", "V4": "claude-sonnet-4-6"}
COEF = REPO / "crates" / "crush-core" / "src" / "estimate_coef.rs"
NAMES = ["one", "bytes", "letter_runs", "letters_long", "camel", "upper", "digit_runs", "digits_long", "punct",
         "punct_runs", "space_runs", "spaces_in_runs", "newlines", "nonascii", "mixed_runs", "mixed_chars"]


def nnls(A, b, iters=3000):
    """Lawson-Hanson non-negative least squares (small problems)."""
    m, n = A.shape
    P, R = [], list(range(n))
    x = np.zeros(n)
    w = A.T @ (b - A @ x)
    for _ in range(iters):
        if not R or max(w[R]) <= 1e-10:
            break
        j = R[int(np.argmax(w[R]))]
        P.append(j)
        R.remove(j)
        while True:
            s = np.zeros(n)
            s[P] = np.linalg.lstsq(A[:, P], b, rcond=None)[0]
            if min(s[P]) > 0:
                x = s
                break
            neg = [i for i in P if s[i] <= 0]
            alpha = min(x[i] / (x[i] - s[i]) for i in neg)
            x = x + alpha * (s - x)
            for i in [i for i in P if abs(x[i]) < 1e-12]:
                P.remove(i)
                R.append(i)
        w = A.T @ (b - A @ x)
    return x


def load():
    ids = json.loads((CORPUS_DIR / "estimator-ids.json").read_text())
    want = set(ids)
    rows = [{"id": r["id"], "class": r["class"], "output": r["output"], "set": "corpus"}
            for r in read_jsonl(CORPUS_DIR / "corpus.jsonl") if r["id"] in want]
    rows += [{"id": s["id"], "class": s["class"], "output": s["output"], "set": "fixtures"} for s in load_samples("fixtures")]
    db = sqlite3.connect(CORPUS_DIR / "token-cache.sqlite")
    for r in rows:
        h = sha(r["output"])
        for k, m in MODELS.items():
            x = db.execute("SELECT n FROM t WHERE model=? AND h=?", (m, h)).fetchone()
            r[k] = x[0] if x else None
    rows = [r for r in rows if r["V5"] and r["V4"] and r["output"]]
    p = subprocess.run([str(DRIVER), "--features"], input="".join(
        json.dumps({"id": r["id"], "tool": "", "input": {}, "output": r["output"]}) + "\n" for r in rows),
        capture_output=True, text=True, check=True)
    feats = {json.loads(l)["id"]: json.loads(l)["f"] for l in p.stdout.splitlines()}
    for r in rows:
        r["f"] = feats[r["id"]]
    return rows


def split(rows):
    test = lambda r: int(hashlib.sha256(f"est:{r['id']}".encode()).hexdigest(), 16) % 5 == 0
    return [r for r in rows if not test(r)], [r for r in rows if test(r)]


def score(rows, w, k):
    X = np.array([r["f"] for r in rows], float)
    y = np.array([r[k] for r in rows], float)
    est = np.maximum(1, np.round(X @ w))
    rel = (est - y) / y
    big = y >= 500
    out = {"n": len(rows), "sum_ratio": round(float(est.sum() / y.sum()), 4),
           "median_abs_rel_err": round(float(np.median(np.abs(rel))), 4),
           "p90_abs_rel_err": round(float(np.quantile(np.abs(rel), .9)), 4),
           "p95_abs_rel_err": round(float(np.quantile(np.abs(rel), .95)), 4),
           "n_ge500": int(big.sum()),
           "median_abs_rel_err_ge500": round(float(np.median(np.abs(rel[big]))), 4) if big.any() else None,
           "p95_abs_rel_err_ge500": round(float(np.quantile(np.abs(rel[big]), .95)), 4) if big.any() else None}
    byc = defaultdict(lambda: [0.0, 0.0])
    for r, e in zip(rows, est):
        byc[r["class"]][0] += e
        byc[r["class"]][1] += r[k]
    out["sum_ratio_by_class"] = {c: round(float(a / b), 3) for c, (a, b) in sorted(byc.items())}
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", action="store_true")
    args = ap.parse_args()
    rows = load()
    train, test = split(rows)
    rep = {"samples": len(rows), "train": len(train), "test": len(test), "models": MODELS, "fit": {}}
    coefs = {}
    for k in MODELS:
        if args.check:
            txt = COEF.read_text()
            line = next(l for l in txt.splitlines() if l.startswith(f"const COEF_{k}"))
            w = np.array([float(x) for x in line.split("[", 2)[2].split("]")[0].split(",")])
        else:
            X = np.array([r["f"] for r in train], float)
            y = np.array([r[k] for r in train], float)
            sw = 1 / np.sqrt(np.maximum(y, 20))  # relative-error weighting
            w = nnls(X * sw[:, None], y * sw)
        coefs[k] = w
        rep["fit"][k] = {"weights": dict(zip(NAMES, [round(float(x), 5) for x in w])),
                         "train": score(train, w, k), "test": score(test, w, k)}
    if not args.check:
        body = "\n".join(f"const COEF_{k}: [f64; K] = [{', '.join(f'{x:.6f}' for x in coefs[k])}];" for k in MODELS)
        COEF.write_text("// @generated by eval/estimator/fit.py from real count_tokens numbers; do not edit.\n"
                        "// Feature order: crate::estimate::FEATURE_NAMES. Held-out error: eval/out/estimator.json.\n"
                        + body + "\n")
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "estimator.json").write_text(json.dumps(rep, indent=1))
    for k in MODELS:
        t = rep["fit"][k]["test"]
        print(f"{k} {MODELS[k]}: held-out n={t['n']} sum ratio {t['sum_ratio']} median |err| {t['median_abs_rel_err']:.1%} "
              f"p90 {t['p90_abs_rel_err']:.1%} p95 {t['p95_abs_rel_err']:.1%}; >=500 tok: median {t['median_abs_rel_err_ge500']:.1%} "
              f"p95 {t['p95_abs_rel_err_ge500']:.1%}")
        print("   weights:", rep["fit"][k]["weights"])
        print("   by class:", t["sum_ratio_by_class"])
    return 0


if __name__ == "__main__":
    sys.exit(main())
