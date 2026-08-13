#!/usr/bin/env python3
"""Contract suite runner (capture mode) — Wave R, R-1. Python 3 stdlib only.

Starts a given server binary on a given port against a temp working directory
(seeded SQLite DB + minimal web/dist), runs every fixture in contract/cases/,
and records actual responses to contract/golden/<case>.json.

Server lifecycle per env group is THREE phases:
  1. migrate: start the binary on an empty DB, wait for /healthz, stop it
     (both the Go and the Rust server run migrations before binding).
  2. seed:    apply contract/seed.sql with sqlite3.
  3. serve:   start the binary again and run the group's cases.

If any case's "expect" contradicts the actual response, NOTHING is written and
the exit code is 1 — the golden set is only ever written from a fully
consistent run, so a bad fixture or wrong binary can never poison it.
"""
import argparse
import base64
import hashlib
import hmac
import http.client
import http.server
import json
import os
import shutil
import socket
import sqlite3
import subprocess
import tempfile
import threading
import time
import re

# Exact byte string written to web/dist/index.html in the temp working dir.
# The spa_root fixture asserts this verbatim.
INDEX_HTML = "<!doctype html><html><head><title>contract</title></head><body>contract spa</body></html>\n"

RECORDED_HEADERS = (
    "content-type",
    "set-cookie",
    "location",
    "access-control-allow-origin",
    "access-control-allow-credentials",
    "access-control-allow-methods",
    "access-control-allow-headers",
    "vary",
    "x-ratelimit-remaining",
    "x-content-type-options",
)

DEFAULT_SIGNING_KEY = "contract-test-signing-key-0123456789abcdef0123456789"
DEFAULT_WEBHOOK_SECRET = "whsec_contract_test"
SESSION_COOKIE_MAX_AGE = 604800  # 7 days, matches Go SessionManager.maxAge

HERE = os.path.dirname(os.path.abspath(__file__))


# ---------- Go-compatible signing helpers ----------

def b64url(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode("ascii")


def fabricate_session_cookie(signing_key: str, fields: dict) -> str:
    """Build a cookie byte-identical to Go's SessionManager.Create:
    base64url(json payload) + "." + base64url(HMAC-SHA256(key, payload)).
    Field order matches the Go sessionPayload struct."""
    payload = {
        "uid": fields["uid"],
        "email": fields["email"],
        "name": fields["name"],
        "groups": fields["groups"],
        "premium": fields["premium"],
        "exp": int(time.time()) + SESSION_COOKIE_MAX_AGE,
    }
    raw = json.dumps(payload, separators=(",", ":")).encode()
    sig = hmac.new(signing_key.encode(), raw, hashlib.sha256).digest()
    return b64url(raw) + "." + b64url(sig)


def stripe_signature_header(secret: str, body: bytes) -> str:
    ts = int(time.time())
    sig = hmac.new(secret.encode(), str(ts).encode() + b"." + body, hashlib.sha256).hexdigest()
    return f"t={ts},v1={sig}"


# ---------- local OIDC discovery stub (provider build needs it offline) ----------

class OidcStub:
    def __init__(self):
        outer = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                if self.path == "/.well-known/openid-configuration":
                    issuer = f"http://127.0.0.1:{outer.port}"
                    doc = {
                        "issuer": issuer,
                        "authorization_endpoint": issuer + "/authorize",
                        "token_endpoint": issuer + "/token",
                        "userinfo_endpoint": issuer + "/userinfo",
                        "jwks_uri": issuer + "/jwks",
                        # Mandatory OpenID Provider Metadata fields. go-oidc
                        # ignores them, but openidconnect 3.5 (Rust chassis)
                        # refuses to parse the metadata document without them.
                        "response_types_supported": ["code"],
                        "subject_types_supported": ["public"],
                        "id_token_signing_alg_values_supported": ["RS256"],
                    }
                    data = json.dumps(doc).encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                elif self.path == "/jwks":
                    # openidconnect 3.5 (Rust chassis) fetches jwks_uri eagerly
                    # during discovery; go-oidc fetches lazily, so the Go server
                    # never hits this. An empty key set suffices: no fixture
                    # verifies tokens (the callback path is not capturable
                    # offline — see README).
                    data = b'{"keys":[]}'
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                else:
                    self.send_response(404)
                    self.send_header("Content-Length", "0")
                    self.end_headers()

            def log_message(self, *args):
                pass

        self.httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.port = self.httpd.server_address[1]
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)

    def start(self):
        self.thread.start()

    def stop(self):
        self.httpd.shutdown()
        self.httpd.server_close()


# ---------- server lifecycle ----------

def start_server(binary, port, env, workdir, db_name="contract.db"):
    full_env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": os.environ.get("HOME", "/tmp"),
        "API_PORT": str(port),
        # Go reads DATABASE_URL; the Rust chassis (delta 10) reads DATABASE_PATH.
        "DATABASE_URL": f"file:{workdir}/{db_name}?_pragma=foreign_keys(1)",
        "DATABASE_PATH": f"{workdir}/{db_name}",
    }
    full_env.update(env)
    return subprocess.Popen(
        [binary],
        cwd=workdir,
        env=full_env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def wait_healthz(port, timeout=15.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            conn = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
            conn.request("GET", "/healthz")
            resp = conn.getresponse()
            resp.read()
            conn.close()
            if resp.status == 200:
                return True
        except OSError:
            time.sleep(0.1)
    return False


def stop_server(proc):
    proc.terminate()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()


def make_workdir():
    workdir = tempfile.mkdtemp(prefix="contract-")
    dist = os.path.join(workdir, "web", "dist")
    os.makedirs(dist)
    with open(os.path.join(dist, "index.html"), "w") as f:
        f.write(INDEX_HTML)
    return workdir


def seed_db(db_path, seed_path):
    with open(seed_path) as f:
        sql = f.read()
    conn = sqlite3.connect(db_path)
    try:
        conn.executescript(sql)
        conn.commit()
    finally:
        conn.close()


def subst_env(env, mapping):
    out = {}
    for k, v in env.items():
        if isinstance(v, str):
            for ph, val in mapping.items():
                v = v.replace(ph, val)
        out[k] = v
    return out


# ---------- request execution ----------

def normalize_volatile(value, port, env):
    """Replace run-varying values (server port, OIDC stub port, random OIDC
    state) with stable placeholders so goldens are deterministic and verify
    can compare responses byte-exactly across runs and ports."""
    if not isinstance(value, str):
        return value
    value = value.replace(f"127.0.0.1%3A{port}", "127.0.0.1%3A{PORT}")
    value = value.replace(f"127.0.0.1:{port}", "127.0.0.1:{PORT}")
    m = re.search(r"127\.0\.0\.1:(\d+)", env.get("OIDC_ISSUER", ""))
    if m:
        oidc_port = m.group(1)
        value = value.replace(f"127.0.0.1%3A{oidc_port}", "127.0.0.1%3A{OIDC_PORT}")
        value = value.replace(f"127.0.0.1:{oidc_port}", "127.0.0.1:{OIDC_PORT}")
    value = re.sub(r"oidc_state=[A-Za-z0-9_-]+", "oidc_state={STATE}", value)
    value = re.sub(r"state=[A-Za-z0-9_-]+", "state={STATE}", value)
    # Random API key plaintexts minted by POST /api/keys (pk_ + 16 random
    # bytes hex-encoded) are replaced so goldens compare deterministically.
    value = re.sub(r"pk_[0-9a-f]{32}", "{KEY}", value)
    return value


def run_request(port, case, env):
    body = case.get("body")
    raw = body.encode() if isinstance(body, str) else (
        json.dumps(body).encode() if body is not None else None
    )
    headers = dict(case.get("headers", {}))
    if case.get("stripe_sign"):
        secret = env.get("STRIPE_WEBHOOK_SECRET", DEFAULT_WEBHOOK_SECRET)
        headers["Stripe-Signature"] = stripe_signature_header(secret, raw or b"")
    if case.get("session_cookie"):
        key = env.get("SESSION_SIGNING_KEY", DEFAULT_SIGNING_KEY)
        headers["Cookie"] = "session=" + fabricate_session_cookie(key, case["session_cookie"])
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    conn.request(case["method"], case["path"], body=raw, headers=headers)
    resp = conn.getresponse()
    data = resp.read()
    hdrs = {}
    for name, value in resp.getheaders():
        name = name.lower()
        if name not in RECORDED_HEADERS:
            continue
        if name in hdrs:
            prev = hdrs[name]
            if isinstance(prev, list):
                prev.append(value)
            else:
                hdrs[name] = [prev, value]
        else:
            hdrs[name] = value
    conn.close()
    hdrs = {
        k: ([normalize_volatile(v, port, env) for v in vs] if isinstance(vs, list)
            else normalize_volatile(vs, port, env))
        for k, vs in hdrs.items()
    }
    text = normalize_volatile(data.decode("utf-8", errors="replace"), port, env)
    try:
        parsed = json.loads(text)
    except json.JSONDecodeError:
        parsed = None
    # body is the parsed JSON value when parseable, else the raw text;
    # the raw text is returned alongside so "text" expectations match
    # bytes exactly even for JSON-parseable bodies.
    body_out = parsed if parsed is not None else text
    return {"status": resp.status, "headers": hdrs, "body": body_out}, text


def run_case(port, case, env):
    actual = None
    raw = None
    for _ in range(int(case.get("repeat", 1))):
        actual, raw = run_request(port, case, env)
    return actual, raw


# ---------- expectation matching ----------

def _join(value):
    return "\n".join(value) if isinstance(value, list) else value


def match_headers(expect_headers, actual_headers, diffs, where):
    for name, rule in expect_headers.items():
        name = name.lower()
        actual = _join(actual_headers.get(name))
        if isinstance(rule, dict):
            if rule.get("absent"):
                if actual is not None:
                    diffs.append(f"{where}: header {name} expected absent, got {actual!r}")
            elif "contains" in rule:
                if actual is None or rule["contains"] not in actual:
                    diffs.append(f"{where}: header {name} expected to contain {rule['contains']!r}, got {actual!r}")
            elif "contains_all" in rule:
                for s in rule["contains_all"]:
                    if actual is None or s not in actual:
                        diffs.append(f"{where}: header {name} expected to contain {s!r}, got {actual!r}")
            elif "prefix" in rule:
                if actual is None or not actual.startswith(rule["prefix"]):
                    diffs.append(f"{where}: header {name} expected prefix {rule['prefix']!r}, got {actual!r}")
            else:
                diffs.append(f"{where}: header {name} has unknown matcher {rule!r}")
        elif actual != rule:
            diffs.append(f"{where}: header {name} expected {rule!r}, got {actual!r}")


def subset(expected, actual):
    if isinstance(expected, dict):
        return isinstance(actual, dict) and all(
            k in actual and subset(v, actual[k]) for k, v in expected.items()
        )
    if isinstance(expected, list):
        return (
            isinstance(actual, list)
            and len(expected) == len(actual)
            and all(subset(e, a) for e, a in zip(expected, actual))
        )
    return expected == actual


def match_expect(expect, actual, raw, where):
    diffs = []
    if "status" in expect and actual["status"] != expect["status"]:
        diffs.append(f"{where}: status expected {expect['status']}, got {actual['status']}")
    match_headers(expect.get("headers", {}), actual["headers"], diffs, where)
    body = actual["body"]
    if "json" in expect and body != expect["json"]:
        diffs.append(
            f"{where}: json mismatch\n  expected: {json.dumps(expect['json'], sort_keys=True)}\n"
            f"  actual:   {json.dumps(body, sort_keys=True) if not isinstance(body, str) else body!r}"
        )
    if "json_subset" in expect and not subset(expect["json_subset"], body):
        diffs.append(
            f"{where}: json_subset mismatch\n  subset:  {json.dumps(expect['json_subset'], sort_keys=True)}\n"
            f"  actual:  {json.dumps(body, sort_keys=True) if not isinstance(body, str) else body!r}"
        )
    if "text" in expect and raw != expect["text"]:
        diffs.append(f"{where}: text expected {expect['text']!r}, got {raw!r}")
    return diffs


# ---------- case loading and grouping ----------

def load_cases(cases_dir):
    cases = []
    for fn in sorted(os.listdir(cases_dir)):
        if fn.endswith(".json"):
            with open(os.path.join(cases_dir, fn)) as f:
                cases.append(json.load(f))
    return cases


def group_cases(cases):
    """Group by env; isolate cases each get their own group. Within a group,
    non-slow cases run first (sorted by name), slow cases last. Groups with a
    slow member run after all others."""
    by_env = {}
    isolated = []
    for case in cases:
        if case.get("isolate"):
            isolated.append([case])
        else:
            key = json.dumps(case.get("env", {}), sort_keys=True)
            by_env.setdefault(key, []).append(case)
    groups = []
    for key in sorted(by_env):
        members = by_env[key]
        members.sort(key=lambda c: (bool(c.get("slow")), c["name"]))
        groups.append(members)
    groups.extend(isolated)
    groups.sort(key=lambda g: any(c.get("slow") for c in g))
    return groups


def ensure_port_free(port):
    """Fail fast if something already listens on the suite port. A stale
    server makes wait_healthz pass against a FOREIGN process and poisons the
    run at seed time (observed twice during Wave R execution)."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        if s.connect_ex(("127.0.0.1", port)) == 0:
            raise RuntimeError(
                f"suite port {port} is already bound by another process — "
                "kill the stale server before running the contract suite"
            )


def run_group(binary, port, cases, seed_path, stub, expect_fn=None, db_name="contract.db"):
    """Run one env group. Returns (results, diffs). expect_fn(case) -> expect
    dict; default is case['expect']."""
    if expect_fn is None:
        expect_fn = lambda c: c.get("expect", {})
    ensure_port_free(port)
    workdir = make_workdir()
    results = {}
    diffs = []
    mapping = {"{PORT}": str(port), "{OIDC_PORT}": str(stub.port) if stub else "0"}
    env = subst_env(cases[0].get("env", {}), mapping)
    try:
        proc = start_server(binary, port, env, workdir, db_name)
        if not wait_healthz(port):
            stop_server(proc)
            raise RuntimeError(f"server failed to start in migrate phase (group env {env})")
        stop_server(proc)
        time.sleep(0.3)
        seed_db(os.path.join(workdir, db_name), seed_path)
        proc = start_server(binary, port, env, workdir, db_name)
        if not wait_healthz(port):
            stop_server(proc)
            raise RuntimeError(f"server failed to start in serve phase (group env {env})")
        try:
            for case in cases:
                actual, raw = run_case(port, case, env)
                results[case["name"]] = actual
                diffs.extend(match_expect(expect_fn(case), actual, raw, case["name"]))
        finally:
            stop_server(proc)
            time.sleep(0.3)
    finally:
        shutil.rmtree(workdir, ignore_errors=True)
    return results, diffs


def needs_stub(cases):
    return any("{OIDC_PORT}" in json.dumps(c.get("env", {})) for c in cases)


def main():
    ap = argparse.ArgumentParser(description="Capture golden contract fixtures from a server binary.")
    ap.add_argument("--binary", required=True, help="path to the server binary under test")
    ap.add_argument("--port", type=int, default=18080)
    ap.add_argument("--cases", default=os.path.join(HERE, "cases"))
    ap.add_argument("--golden", default=os.path.join(HERE, "golden"))
    ap.add_argument("--seed", default=os.path.join(HERE, "seed.sql"))
    args = ap.parse_args()

    cases = load_cases(args.cases)
    stub = OidcStub() if needs_stub(cases) else None
    if stub:
        stub.start()
    all_results = {}
    failures = []
    try:
        for group in group_cases(cases):
            results, diffs = run_group(args.binary, args.port, group, args.seed, stub)
            all_results.update(results)
            failures.extend(diffs)
    finally:
        if stub:
            stub.stop()

    if failures:
        print("EXPECT MISMATCHES — golden NOT written; fix the fixture or the binary:")
        for d in failures:
            print(" -", d)
        return 1

    os.makedirs(args.golden, exist_ok=True)
    for name in sorted(all_results):
        out = os.path.join(args.golden, name + ".json")
        with open(out, "w") as f:
            json.dump(all_results[name], f, indent=2, sort_keys=True)
            f.write("\n")
    print(f"capture ok: {len(all_results)} cases -> {args.golden}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
