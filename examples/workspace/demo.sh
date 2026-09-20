#!/usr/bin/env bash
# Semantic-workspace demo: multi-module resolution, rename
# transactions (preview / apply / rejections), daemon ops, and
# shifted-edit incremental evidence.
#
# Runs against a *copy* of this directory in a temp dir — the apply
# step rewrites the copies, never the checked-in fixtures.
#
#   ./examples/workspace/demo.sh                 # uses ontixa/ontixad on PATH
#   ONTIXA=target/debug/ontixa ONTIXAD=target/debug/ontixad \
#       ./examples/workspace/demo.sh             # or explicit binaries
#
set -uo pipefail

ONTIXA=${ONTIXA:-ontixa}
ONTIXAD=${ONTIXAD:-ontixad}
HERE=$(cd "$(dirname "$0")" && pwd)
# Resolve binaries to absolute paths before cd'ing into the temp dir.
# Falls back to the repo's target/debug build when the name isn't on
# PATH, so `./demo.sh` works out of the box after `cargo build`.
resolve() {
  case $1 in
    */*) readlink -f "$1" ;;      # an explicit path — absolutize it
    *)
      command -v "$1" && return
      for suffix in "$1" "$1.exe"; do
        [ -x "$HERE/../../target/debug/$suffix" ] &&
          { readlink -f "$HERE/../../target/debug/$suffix"; return; }
      done
      printf 'cannot find %s on PATH or in target/debug\n' "$1" >&2
      exit 1 ;;
  esac
}
ONTIXA=$(resolve "$ONTIXA")
ONTIXAD=$(resolve "$ONTIXAD")
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cp "$HERE"/main.ixa "$HERE"/math.ixa "$WORK"/
# Windows binaries can't read MSYS-style paths (/tmp/...): hand them
# a drive-letter path with forward slashes (valid + JSON-safe).
WWORK=$(cygpath -m "$WORK" 2>/dev/null || printf '%s' "$WORK")

say() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
run() { printf '\n$ %s\n' "$*"; "$@"; }

cd "$WORK"

say "1. The workspace: main.ixa uses math + math::Vec2"
cat math.ixa

say "2. Run the multi-module program — double(norm(3,4))"
run "$ONTIXA" run main.ixa

say "3. Explain a *qualified* symbol — resolution, not text search"
run "$ONTIXA" explain main.ixa math::double

say "4. Rename preview — edits per file, qualifier preserved"
run "$ONTIXA" rename main.ixa math::double twice

say "5. Rename apply — both files land in one revision"
run "$ONTIXA" rename main.ixa math::double twice --apply
run "$ONTIXA" run main.ixa

say "6. Rejected renames — nothing mutates"
printf '\n$ ontixa rename main.ixa math::twice norm   # conflict: math already binds `norm`\n'
"$ONTIXA" rename main.ixa math::twice norm; echo "exit=$?"
printf '\n$ ontixa rename main.ixa math::twice "not a name"   # invalid identifier\n'
"$ONTIXA" rename main.ixa math::twice "not a name"; echo "exit=$?"
printf '\n$ ontixa rename main.ixa math::nope x   # unknown symbol\n'
"$ONTIXA" rename main.ixa math::nope x; echo "exit=$?"
grep -c 'fn twice' math.ixa | sed 's/^/math.ixa still has `fn twice` x/'

say "7. Daemon: rename preview → apply → shifted-edit → stale"

# A `set` request for math.ixa with a leading comment — the shifted
# edit. JSON escaping needs python (python3 or python).
SHIFTED_SET=$WORK/set-shifted.jsonl
{ python3 - "$WWORK" 2>/dev/null || python - "$WWORK"; } <<'PY' > "$SHIFTED_SET"
import json, sys
# newline='' keeps the file's CRLF endings — a plain text-mode read
# would translate \r\n → \n and smuggle a line-ending diff into the
# `set` text, which the engine would (correctly) treat as a body edit
# rather than a pure offset shift.
src = open(sys.argv[1] + "/math.ixa", newline="").read()
print(json.dumps({"op": "set", "path": sys.argv[1] + "/math.ixa",
                  "text": "// shifted\n" + src}))
PY

{ printf '%s\n' "{\"op\":\"open\",\"path\":\"$WWORK/main.ixa\"}"
  printf '%s\n' "{\"op\":\"rename\",\"path\":\"$WWORK/main.ixa\",\"symbol\":\"math::twice\",\"to\":\"triple\"}"
  printf '%s\n' '{"op":"shutdown"}'
} | "$ONTIXAD" > "$WORK/out1.jsonl"
REV=$(grep -o '"revision":[0-9]*' "$WORK/out1.jsonl" | head -1 | cut -d: -f2)
printf 'planned revision: %s\n' "$REV"

printf '\n-- apply with the WRONG revision (rev+9) → stale, nothing mutates\n'
{ printf '%s\n' "{\"op\":\"open\",\"path\":\"$WWORK/main.ixa\"}"
  printf '%s\n' "{\"op\":\"rename\",\"path\":\"$WWORK/main.ixa\",\"symbol\":\"math::twice\",\"to\":\"triple\",\"apply\":true,\"revision\":$((REV+9))}"
  printf '%s\n' '{"op":"shutdown"}'
} | "$ONTIXAD" | grep -o 'E_STALE_REVISION' | head -1

printf '\n-- apply with the planned revision → applied, new_sources returned\n'
{ printf '%s\n' "{\"op\":\"open\",\"path\":\"$WWORK/main.ixa\"}"
  printf '%s\n' "{\"op\":\"rename\",\"path\":\"$WWORK/main.ixa\",\"symbol\":\"math::twice\",\"to\":\"triple\",\"apply\":true,\"revision\":$REV}"
  printf '%s\n' '{"op":"shutdown"}'
} | "$ONTIXAD" | grep -o '"applied":true'

printf '\n-- shifted edit: prepend a comment to math.ixa via `set`, then `check`\n'
printf '   every absolute offset moved — evaluated queries stay small:\n'
{ printf '%s\n' "{\"op\":\"open\",\"path\":\"$WWORK/main.ixa\"}"
  printf '%s\n' "{\"op\":\"check\",\"path\":\"$WWORK/main.ixa\"}"
  cat "$SHIFTED_SET"
  printf '%s\n' "{\"op\":\"check\",\"path\":\"$WWORK/main.ixa\"}"
  printf '%s\n' '{"op":"shutdown"}'
} | "$ONTIXAD" > "$WORK/out2.jsonl"
tail -1 "$WORK/out2.jsonl" | grep -o '"evaluated":\[[^]]*\]'

say "8. Fixture files untouched — demo ran on copies"
grep -l 'fn double' "$HERE"/math.ixa | sed 's/^/original still declares `double`: /'
