#!/bin/bash
# Self-contained test for deploy.sh rollback behavior.
# Stubs ssh/rsync/curl/cargo/npm/npx/systemctl/sleep on PATH ahead of the real
# ones; a temp dir stands in for the remote filesystem (ssh runs the "remote"
# command locally, rsync copies locally, DEPLOY_BINARY/DEPLOY_PATH point into
# the temp dir). Prints PASS/FAIL per assertion; exits non-zero on any failure.
set -u
cd "$(dirname "$0")"
DEPLOY_SH="$PWD/deploy.sh"

FAILURES=0
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT

STUBS="$T/stubs"
mkdir -p "$STUBS"

cat > "$STUBS/ssh" <<'EOF'
#!/bin/bash
# Run the "remote" command (last arg) locally.
bash -c "${@: -1}"
EOF

cat > "$STUBS/rsync" <<'EOF'
#!/bin/bash
# Local-copy stand-in; dest is given as host:path; supports --delete and -e.
delete=0
pos=()
skip=0
for a in "$@"; do
  if [ "$skip" = 1 ]; then skip=0; continue; fi
  case "$a" in
    --delete) delete=1 ;;
    -e) skip=1 ;;
    -*) ;;
    *) pos+=("$a") ;;
  esac
done
src="${pos[0]}"
dst="${pos[1]#*:}"
if [ "$delete" = 1 ]; then
  rm -rf "$dst"
  mkdir -p "$dst"
  cp -r "$src". "$dst"
else
  mkdir -p "$(dirname "$dst")"
  cp -r "$src" "$dst"
fi
EOF

cat > "$STUBS/curl" <<'EOF'
#!/bin/bash
# Fail the first $CURL_FAILS invocations, then succeed.
n=0
[ -f "$CURL_COUNT_FILE" ] && n="$(cat "$CURL_COUNT_FILE")"
n=$((n + 1))
echo "$n" > "$CURL_COUNT_FILE"
[ "$n" -gt "$CURL_FAILS" ]
EOF

cat > "$STUBS/cargo" <<'EOF'
#!/bin/bash
# Fake `cargo build --release`: write the new-release marker where deploy.sh
# expects the server binary (cwd is service/).
mkdir -p target/release
echo "v2-binary" > target/release/server
exit 0
EOF

cat > "$STUBS/npm" <<'EOF'
#!/bin/bash
exit 0
EOF

cat > "$STUBS/npx" <<'EOF'
#!/bin/bash
# Fake `nuxt generate`: emit the new-release web tree (cwd is web/).
mkdir -p .output/public
echo "new-web" > .output/public/index.html
exit 0
EOF

cat > "$STUBS/systemctl" <<'EOF'
#!/bin/bash
[ "${FAIL_SYSTEMCTL:-0}" = "1" ] && exit 1
exit 0
EOF

cat > "$STUBS/sleep" <<'EOF'
#!/bin/bash
exit 0
EOF

chmod +x "$STUBS"/*

pass() { echo "PASS: $1"; }
fail() { echo "FAIL: $1 — $2"; FAILURES=$((FAILURES + 1)); }

assert_eq() { # desc actual expected
  if [ "$2" = "$3" ]; then pass "$1"; else fail "$1" "expected [$3] got [$2]"; fi
}
assert_grep() { # desc pattern haystack
  if grep -qF -- "$2" <<< "$3"; then pass "$1"; else fail "$1" "missing [$2]"; fi
}
assert_nogrep() { # desc pattern haystack
  if grep -qF -- "$2" <<< "$3"; then fail "$1" "unexpected [$2]"; else pass "$1"; fi
}
assert_absent() { # desc path
  if [ -e "$2" ]; then fail "$1" "$2 still exists"; else pass "$1"; fi
}

setup_case() { # case-name
  CASE="$T/$1"
  CLONE="$CASE/clone"
  REMOTE_ROOT="$CASE/remote"
  mkdir -p "$CLONE/service" "$CLONE/web" "$REMOTE_ROOT"
  touch "$CASE/deploy_key"
  export CURL_COUNT_FILE="$CASE/curl_count"
  export CURL_FAILS=999
  export FAIL_SYSTEMCTL=0
}

seed_release() { # pre-existing remote release (binary v1 + old web dist)
  mkdir -p "$REMOTE_ROOT/bin" "$REMOTE_ROOT/app/web/dist"
  echo "v1-binary" > "$REMOTE_ROOT/bin/testapp-server"
  echo "old-web" > "$REMOTE_ROOT/app/web/dist/index.html"
}

run_deploy() { # stdout+stderr captured by caller; rc in $?
  (
    cd "$CLONE"
    PATH="$STUBS:$PATH" \
    DEPLOY_HOST=fake DEPLOY_USER=fake \
    DEPLOY_PATH="$REMOTE_ROOT/app" \
    DEPLOY_DOMAIN=example.com \
    DEPLOY_BINARY="$REMOTE_ROOT/bin/testapp-server" \
    DEPLOY_KEY_FILE="$CASE/deploy_key" \
    bash "$DEPLOY_SH" testapp
  )
}

echo "--- Case 1: health-gate failure -> rollback restores binary+dist, ROLLBACK OK ---"
setup_case case1
seed_release
export CURL_FAILS=10   # fail all 10 gate attempts; succeed on re-verify
out="$(run_deploy 2>&1)"; rc=$?
[ "$rc" -ne 0 ] && pass "case1: exits non-zero" || fail "case1: exits non-zero" "rc=$rc"
assert_grep "case1: prints ROLLBACK OK" "ROLLBACK OK" "$out"
assert_eq "case1: binary rolled back to v1" "$(cat "$REMOTE_ROOT/bin/testapp-server")" "v1-binary"
assert_eq "case1: web dist rolled back to old" "$(cat "$REMOTE_ROOT/app/web/dist/index.html")" "old-web"
assert_absent "case1: binary.prev consumed" "$REMOTE_ROOT/bin/testapp-server.prev"
assert_absent "case1: dist.prev consumed" "$REMOTE_ROOT/app/web/dist.prev"
assert_absent "case1: dist.new consumed by atomic swap" "$REMOTE_ROOT/app/web/dist.new"

echo "--- Case 2: activation ssh failure -> rollback path runs ---"
setup_case case2
seed_release
export CURL_FAILS=0
export FAIL_SYSTEMCTL=1  # activation restart fails; rollback restart fails too (|| true)
out="$(run_deploy 2>&1)"; rc=$?
[ "$rc" -ne 0 ] && pass "case2: exits non-zero" || fail "case2: exits non-zero" "rc=$rc"
assert_grep "case2: activation failure reported" "Activation FAILED" "$out"
assert_eq "case2: binary rolled back to v1" "$(cat "$REMOTE_ROOT/bin/testapp-server")" "v1-binary"
assert_eq "case2: web dist rolled back to old" "$(cat "$REMOTE_ROOT/app/web/dist/index.html")" "old-web"
assert_absent "case2: binary.prev consumed" "$REMOTE_ROOT/bin/testapp-server.prev"
assert_absent "case2: dist.prev consumed" "$REMOTE_ROOT/app/web/dist.prev"
assert_grep "case2: prints rollback verdict" "ROLLBACK" "$out"

echo "--- Case 3: rollback re-verify failure -> ROLLBACK FAILED ---"
setup_case case3
seed_release
export CURL_FAILS=999   # healthz never succeeds
out="$(run_deploy 2>&1)"; rc=$?
[ "$rc" -ne 0 ] && pass "case3: exits non-zero" || fail "case3: exits non-zero" "rc=$rc"
assert_grep "case3: prints ROLLBACK FAILED" "ROLLBACK FAILED" "$out"
assert_nogrep "case3: no ROLLBACK OK" "ROLLBACK OK" "$out"
assert_eq "case3: binary still rolled back to v1" "$(cat "$REMOTE_ROOT/bin/testapp-server")" "v1-binary"
assert_eq "case3: web dist still rolled back to old" "$(cat "$REMOTE_ROOT/app/web/dist/index.html")" "old-web"

echo "--- Case 4: first deploy (no prev) + health failure -> no restore, ROLLBACK FAILED with note ---"
setup_case case4
export CURL_FAILS=999
out="$(run_deploy 2>&1)"; rc=$?
[ "$rc" -ne 0 ] && pass "case4: exits non-zero" || fail "case4: exits non-zero" "rc=$rc"
assert_grep "case4: prints no-previous-release note" "no previous release to restore" "$out"
assert_grep "case4: prints ROLLBACK FAILED" "ROLLBACK FAILED" "$out"
assert_absent "case4: no binary.prev created" "$REMOTE_ROOT/bin/testapp-server.prev"
assert_absent "case4: no dist.prev created" "$REMOTE_ROOT/app/web/dist.prev"
assert_eq "case4: new binary left in place (nothing to restore)" "$(cat "$REMOTE_ROOT/bin/testapp-server")" "v2-binary"
assert_eq "case4: new web dist left in place (nothing to restore)" "$(cat "$REMOTE_ROOT/app/web/dist/index.html")" "new-web"

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "ALL TESTS PASSED"
  exit 0
else
  echo "$FAILURES ASSERTION(S) FAILED"
  exit 1
fi
