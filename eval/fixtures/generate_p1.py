#!/usr/bin/env python3
"""Phase 1 fixtures: shapes the generic transforms target, which the Phase 0
set doesn't cover. Appends to manifest.json (replacing entries of the same
name) and reuses generate.py's runner, sanitiser and leak check.

- rsync_progress   real `rsync --progress` (bandwidth-limited so it redraws
                   with carriage returns) over generated scratch files
- curl_progress    real `curl --progress-bar` on a local file:// URL, rate-limited
- openssl_cert     real `openssl req -x509` PEM output (throwaway key to /dev/null)
- k8s_secret_yaml  `kubectl get secret -o yaml` shape: base64 values from a
                   seeded RNG (no kubectl here; the shape is what matters)
- retry_loop       a real shell retry loop printing the same line many times

No network. Usage: generate_p1.py
"""
from __future__ import annotations

import base64
import json
import random
import shutil
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from generate import DATA, MANIFEST, Gen  # noqa: E402

NAMES = ["rsync_progress", "curl_progress", "openssl_cert", "k8s_secret_yaml", "retry_loop"]


def main() -> int:
    scratch = Path(tempfile.mkdtemp(prefix="crush-fx1-"))
    g = Gen(scratch)
    try:
        (scratch / "src").mkdir()
        rng = random.Random(7)
        for i, size in enumerate((6, 9, 4, 7, 5, 8)):
            (scratch / "src" / f"dataset-{i}.bin").write_bytes(rng.randbytes(size * 1024 * 1024))
        cmd = "rsync -a --progress --bwlimit=3M src/ dst/"
        rc, out = g.sh(cmd + " 2>&1", scratch, timeout=300)
        g.add("rsync_progress", "LISTING", cmd, out, note="real rsync; --bwlimit makes it redraw progress with \\r")

        cmd = "for f in src/dataset-1.bin src/dataset-5.bin; do curl --progress-bar --limit-rate 1500K -o /dev/null file://$PWD/$f; done"
        rc, out = g.sh(cmd + " 2>&1", scratch, timeout=120)
        g.add("curl_progress", "UNKNOWN", cmd.replace("$PWD", "/work"), out, note="real curl progress bar (\\r redraws)")

        cmd = "openssl req -x509 -newkey rsa:2048 -nodes -keyout /dev/null -subj /CN=build.example.test -days 30"
        rc, out = g.sh(cmd + " 2>/dev/null", scratch, timeout=60)
        g.add("openssl_cert", "UNKNOWN", cmd, out, note="real openssl; throwaway key discarded; PEM wrapped at 64")

        r = random.Random(11)
        b64 = lambda n: base64.b64encode(r.randbytes(n)).decode()
        yaml = (
            "apiVersion: v1\n"
            "data:\n"
            f"  ca.crt: {b64(1100)}\n"
            f"  tls.crt: {b64(1300)}\n"
            f"  tls.key: {b64(1700)}\n"
            "  token: " + b64(24) + "\n"
            "kind: Secret\n"
            "metadata:\n"
            "  creationTimestamp: \"2026-10-12T09:41:07Z\"\n"
            "  labels:\n"
            "    app.kubernetes.io/name: payments-api\n"
            "  name: payments-api-tls\n"
            "  namespace: staging\n"
            "  resourceVersion: \"48213977\"\n"
            "  uid: 6f1c2a9e-0b7d-4c55-9a43-1f2e8d3b7c60\n"
            "type: kubernetes.io/tls\n"
        )
        g.add("k8s_secret_yaml", "STRUCTURED", "kubectl get secret payments-api-tls -n staging -o yaml", yaml,
              note="kubectl secret shape; base64 values from a seeded RNG")

        cmd = ("for i in $(seq 1 40); do if [ -e /work/.ready ]; then break; fi; "
               "echo 'waiting for /work/.ready (build lock held by another process)'; done; "
               "echo 'gave up after 40 attempts'; ls /work/.ready 2>&1 | tail -1")
        rc, out = g.sh(cmd.replace("/work", str(scratch)), scratch, timeout=60)
        g.add("retry_loop", "LOG", cmd, out, note="real shell retry loop")
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    man = json.loads(MANIFEST.read_text())
    man["fixtures"] = [f for f in man["fixtures"] if f["name"] not in NAMES] + g.fixtures
    MANIFEST.write_text(json.dumps(man, indent=2) + "\n")
    print(f"manifest: {len(man['fixtures'])} fixtures")
    return 0


if __name__ == "__main__":
    sys.exit(main())
