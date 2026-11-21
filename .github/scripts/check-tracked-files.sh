#!/bin/bash
# No Apple firmware and no large binaries in git (README "Rules"; the names are the ones `oneios-tool package` refuses,
# tools/oneios-tool/crates/oneios-package), and no Python: the repository's tools are oneios-tool (Rust) and POSIX
# shell (CLAUDE.md). A *.py file, or an executable whose first line is a python shebang, fails (a test's stand-in of
# another project's script, not executable, is data: tools/oneios-tool/tests/kernelpy/fixtures/iig.in).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
bad=0
while IFS= read -r -d '' f; do
  n=$(basename "$f" | tr '[:upper:]' '[:lower:]')
  case "$n" in
    kernelcache*|*cryptex*.dmg|*.ipsw|*.dmg|*.aea|*.img|*.simg|*.erofs|*.im4p|*.img4|*.im4m|*.kc|*.trustcache|buildmanifest.plist|restore.plist|*.shsh*|dyld_shared_cache*|*.macho)
      echo "tracked firmware-like file: $f" >&2; bad=1 ;;
    *.py|*.pyc) echo "tracked Python file: $f" >&2; bad=1 ;;
  esac
  [ -f "$f" ] || continue
  if [ -x "$f" ]; then
    case "$(head -c 128 "$f" | tr -d '\000' | head -1)" in
      "#!"*python*) echo "tracked Python script: $f" >&2; bad=1 ;;
    esac
  fi
  if [ "$(stat -c %s "$f")" -gt 5242880 ]; then echo "tracked file over 5 MiB: $f" >&2; bad=1; fi
  case "$(od -An -tx1 -N4 "$f" | tr -d ' \n')" in
    cffaedfe|cefaedfe|cafebabe|cafebabf) echo "tracked Mach-O: $f" >&2; bad=1 ;;
  esac
done < <(git ls-files -z)
exit $bad
