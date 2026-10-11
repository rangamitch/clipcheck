#!/usr/bin/env bash
# Runs every crate's tests one crate at a time with a hard time cap, so a hang is attributed to
# one crate instead of stalling the whole run. The desktop crate (apps/ui/src-tauri) is excluded
# from the root workspace and is its own workspace root, so it is run through --manifest-path.
# Between crates it stops stray test processes, but only ones started from this repo's build
# folders (see kill-test-strays.ps1), never a packaged ClipCheck the user is running.
cd "$(dirname "$0")/.." || exit 1
CAP=${CAP:-600}

kill_strays() {
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts/kill-test-strays.ps1 >/dev/null 2>&1
}

kill_strays
total_p=0
total_f=0
total_s=0
bad=0
for manifest in crates/*/Cargo.toml apps/clipcheckd/Cargo.toml apps/ui/src-tauri/Cargo.toml; do
  p=$(grep -m1 '^name' "$manifest" | cut -d'"' -f2)
  s=$(date +%s)
  timeout "$CAP" cargo test --manifest-path "$manifest" -p "$p" --no-fail-fast -- --test-threads=1 > "/tmp/t-$p.txt" 2>&1
  rc=$?
  e=$(( $(date +%s) - s ))
  read -r pp ff < <(grep -E '^test result' "/tmp/t-$p.txt" | awk '{p+=$4; f+=$6} END {print p+0, f+0}')
  # Tests that could not run (no native test media, WGC, audio device) print a SKIPPED line and pass;
  # count them so the summary shows what did not run. Set CLIPCHECK_REQUIRE_CAPTURE=1 (engine,
  # clipcheckd, capture) or CLIPCHECK_REQUIRE_AUDIO=1 (audio) to make a skip a failure.
  sk=$(grep -c "SKIPPED:" "/tmp/t-$p.txt")
  status=ok
  if [ "$rc" -ne 0 ]; then status="FAIL(rc=$rc)"; bad=1; fi
  printf '%-22s %-12s %4ss  passed %-4s failed %-3s skipped %s\n' "$p" "$status" "$e" "$pp" "$ff" "$sk"
  total_p=$((total_p + pp))
  total_f=$((total_f + ff))
  total_s=$((total_s + sk))
  kill_strays
done
echo "TOTAL passed $total_p failed $total_f skipped $total_s"
exit $bad
