"""Claude token counts via Anthropic `POST /v1/messages/count_tokens`.

Auth (least invasive first; never printed, logged or written anywhere):
  1. ANTHROPIC_API_KEY in the environment  -> `x-api-key`
  2. the OAuth access token Synaps already keeps in ~/.synaps-cli/auth.json
     (`anthropic.access`), read-only. We never refresh it (a refresh would
     rotate the refresh token under Synaps' feet); if it is expired or about to
     expire we stop and ask you to let Synaps refresh it (e.g. run `synaps
     status`). OAuth requests carry the same headers Synaps sends:
     `anthropic-beta: claude-code-20250219,oauth-2025-04-20` and the Claude Code
     identity as the first system block.

What is counted: a single user message whose only content block is the text.
The fixed request overhead (system block + message framing) is measured once
per model with a 1-character probe and subtracted, so `count(text)` is the
tokens of the text itself (+/- 1 at the boundary). Raw-vs-crushed deltas are
exact because the framing cancels.

Cache: sqlite at $CRUSH_CORPUS_DIR/token-cache.sqlite keyed by
(model, sha256(text)) -> tokens. Only hashes and integers are stored.

Rate limit: a shared token bucket (default 4 req/s) across worker threads;
429/529/5xx are retried with exponential backoff honouring `retry-after`.
"""
from __future__ import annotations

import json
import os
import sqlite3
import sys
import threading
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import CORPUS_DIR, private_dir, sha  # noqa: E402

URL = "https://api.anthropic.com/v1/messages/count_tokens"
IDENTITY = "You are Claude Code, Anthropic's official CLI for Claude."
OAUTH_BETA = "claude-code-20250219,oauth-2025-04-20"
# Two tokenizer families exist (measured 2026-10-30): the 4.x family
# (sonnet-4-6 == haiku-4-5 on content) and the 5.x family (opus-5-5 ==
# sonnet-5-5). We count with one model from each.
DEFAULT_MODELS = ["claude-sonnet-4-6", "claude-opus-5-5"]
MAX_TEXT_CHARS = 600_000  # keep requests well under the API's limits


class AuthError(RuntimeError):
    pass


def _auth_headers() -> dict:
    key = os.environ.get("ANTHROPIC_API_KEY")
    if key:
        return {"x-api-key": key, "_kind": "api_key"}
    path = Path(os.environ.get("CRUSH_EVAL_AUTH_FILE", Path.home() / ".synaps-cli" / "auth.json"))
    try:
        a = json.loads(path.read_text())["anthropic"]
    except Exception as e:  # noqa: BLE001
        raise AuthError(f"no Anthropic credential: set ANTHROPIC_API_KEY or log in with synaps ({type(e).__name__})") from None
    if a.get("type") != "oauth" or not a.get("access"):
        raise AuthError("auth.json has no anthropic OAuth access token")
    left = a.get("expires", 0) / 1000 - time.time()
    if left < 600:
        raise AuthError(f"Synaps' Anthropic access token expires in {int(left)}s; "
                        "let Synaps refresh it (e.g. run `synaps status`) and retry. "
                        "This harness never refreshes tokens itself.")
    return {"Authorization": "Bearer " + a["access"], "anthropic-beta": OAUTH_BETA, "_kind": "oauth",
            "_expires": a.get("expires", 0) / 1000}


class Bucket:
    def __init__(self, rate: float):
        self.rate = rate
        self.lock = threading.Lock()
        self.next = time.monotonic()

    def take(self):
        with self.lock:
            now = time.monotonic()
            wait = self.next - now
            self.next = max(now, self.next) + 1.0 / self.rate
        if wait > 0:
            time.sleep(wait)


class TokenCounter:
    def __init__(self, models=None, rate: float | None = None, workers: int = 4, cache_path: Path | None = None):
        self.models = models or DEFAULT_MODELS
        self.bucket = Bucket(rate or float(os.environ.get("CRUSH_TOKENS_RPS", "4")))
        self.workers = workers
        self._hdr = None
        self.calls = 0
        self.retries = 0
        self.cache_hits = 0
        private_dir(CORPUS_DIR)
        self.cache_path = cache_path or (CORPUS_DIR / "token-cache.sqlite")
        new = not self.cache_path.exists()
        self.db = sqlite3.connect(self.cache_path, check_same_thread=False)
        if new:
            os.chmod(self.cache_path, 0o600)
        self.db.execute("CREATE TABLE IF NOT EXISTS t (model TEXT, h TEXT, n INTEGER, PRIMARY KEY(model, h))")
        self.dblock = threading.Lock()
        self.overhead = {}

    # ── transport ──────────────────────────────────────────────────────────
    def _headers(self):
        if self._hdr is None:
            self._hdr = _auth_headers()
        if self._hdr.get("_kind") == "oauth" and self._hdr["_expires"] - time.time() < 120:
            self._hdr = _auth_headers()  # re-read: Synaps may have refreshed it
        return {k: v for k, v in self._hdr.items() if not k.startswith("_")}

    def _api(self, model: str, text: str) -> int:
        body = {"model": model, "messages": [{"role": "user", "content": [{"type": "text", "text": text}]}]}
        if self._headers().get("anthropic-beta"):
            body["system"] = [{"type": "text", "text": IDENTITY}]
        data = json.dumps(body).encode()
        delay = 2.0
        for attempt in range(8):
            self.bucket.take()
            req = urllib.request.Request(URL, data=data, method="POST")
            for k, v in self._headers().items():
                req.add_header(k, v)
            req.add_header("anthropic-version", "2023-06-01")
            req.add_header("content-type", "application/json")
            try:
                with urllib.request.urlopen(req, timeout=60) as r:
                    self.calls += 1
                    return int(json.loads(r.read())["input_tokens"])
            except urllib.error.HTTPError as e:
                self.calls += 1
                if e.code in (429, 500, 502, 503, 504, 529):
                    ra = e.headers.get("retry-after")
                    time.sleep(float(ra) if ra and ra.replace(".", "").isdigit() else delay)
                    delay = min(delay * 2, 60)
                    self.retries += 1
                    continue
                msg = e.read()[:300].decode(errors="replace")
                raise RuntimeError(f"count_tokens HTTP {e.code}: {msg}") from None
            except (urllib.error.URLError, TimeoutError) as e:
                time.sleep(delay)
                delay = min(delay * 2, 60)
                self.retries += 1
                if attempt == 7:
                    raise RuntimeError(f"count_tokens network error: {e}") from None
        raise RuntimeError("count_tokens: retries exhausted")

    def _overhead(self, model: str) -> int:
        if model not in self.overhead:
            # tokens("x") is 1 in every tokenizer we tested; framing = api("x") - 1
            self.overhead[model] = self._api(model, "x") - 1
        return self.overhead[model]

    # ── cache ──────────────────────────────────────────────────────────────
    def _get(self, model, h):
        with self.dblock:
            r = self.db.execute("SELECT n FROM t WHERE model=? AND h=?", (model, h)).fetchone()
        return None if r is None else r[0]

    def _put(self, model, h, n):
        with self.dblock:
            self.db.execute("INSERT OR REPLACE INTO t VALUES (?,?,?)", (model, h, n))
            self.db.commit()

    # ── public ─────────────────────────────────────────────────────────────
    def count(self, text: str, model: str) -> int:
        if not text:
            return 0
        h = sha(text)
        n = self._get(model, h)
        if n is not None:
            self.cache_hits += 1
            return n
        if len(text) > MAX_TEXT_CHARS:
            # Split on a line boundary; additivity holds to ~1-2 tokens/join.
            cut = text.rfind("\n", 0, MAX_TEXT_CHARS)
            cut = MAX_TEXT_CHARS if cut <= 0 else cut + 1
            n = self.count(text[:cut], model) + self.count(text[cut:], model)
        else:
            try:
                n = max(0, self._api(model, text) - self._overhead(model))
            except RuntimeError as e:
                if "HTTP 400" not in str(e) or len(text) < 2000 or "too long" not in str(e).lower():
                    raise
                mid = text.rfind("\n", 0, len(text) // 2)
                mid = len(text) // 2 if mid <= 0 else mid + 1
                n = self.count(text[:mid], model) + self.count(text[mid:], model)
        self._put(model, h, n)
        return n

    def count_many(self, texts, model: str, progress: str = "") -> list[int]:
        texts = list(texts)
        out = [0] * len(texts)
        todo = []
        for i, t in enumerate(texts):
            n = self._get(model, sha(t)) if t else 0
            if n is None:
                todo.append(i)
            else:
                out[i] = n
                self.cache_hits += bool(t)
        if todo:
            self._overhead(model)
            done = [0]
            t0 = time.time()

            def job(i):
                out[i] = self.count(texts[i], model)
                done[0] += 1
                if progress and done[0] % 200 == 0:
                    print(f"    {progress} {model}: {done[0]}/{len(todo)} ({time.time() - t0:.0f}s)", file=sys.stderr)
            with ThreadPoolExecutor(self.workers) as ex:
                list(ex.map(job, todo))
        return out

    def count_batched(self, texts, model: str, max_chars: int = 400_000, sep: str = "\n") -> tuple[int, int]:
        """Sum of tokens over many texts, packed into few requests. Returns
        (total_tokens, requests). Join error is ~1-2 tokens per boundary;
        `calibrate_batching` measures it."""
        batches, cur, size = [], [], 0
        for t in texts:
            if not t:
                continue
            if size + len(t) > max_chars and cur:
                batches.append(sep.join(cur))
                cur, size = [], 0
            cur.append(t)
            size += len(t) + len(sep)
        if cur:
            batches.append(sep.join(cur))
        counts = self.count_many(batches, model)
        return sum(counts), len(batches)

    def stats(self) -> dict:
        return {"api_calls": self.calls, "retries": self.retries, "cache_hits": self.cache_hits,
                "auth": (self._hdr or {}).get("_kind")}


if __name__ == "__main__":
    tc = TokenCounter()
    for m in tc.models:
        print(m, "hello world ->", tc.count("hello world", m), "tokens; overhead", tc._overhead(m))
    print(tc.stats())
