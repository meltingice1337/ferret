#!/usr/bin/env bash
# Builds Ferret's text recognition add-on, ferret-ocr-pack-1.zip, and signs it with the updater
# key, for Ferret to download when asked (ferret-gpui/src/addons.rs). It doesn't publish anything:
# at the end it prints the `gh release create` command that does. Run from Git Bash:
#
#   scripts/build-ocr-pack.sh                  downloads the pinned sources (into the output folder)
#   scripts/build-ocr-pack.sh --cache DIR      takes the sources from DIR where they're there
#   options: --out DIR (default ferret-gpui/target/ocr-pack)
#
# Every source is pinned by version or revision and SHA-256: ONNX Runtime 1.31.1 (Microsoft, MIT)
# and three PaddleOCR PP-OCRv5 models (PaddlePaddle, Apache-2.0, from Hugging Face). A cache
# folder may hold onnxruntime-win-x64-1.31.1.zip or that zip unpacked (also under ort/), and the
# models as <repo>/<file> (also under models/); anything missing is downloaded into it. A file
# whose SHA-256 isn't the pinned one stops the build.
#
# The pack: a zip with flat paths, manifest.json ({"pack":1,"files":{"<path>":"<sha256>"}} for
# every other file), signed as scripts/release.sh signs the installer (`tauri signer sign`; the
# key at ~/.tauri/ferret.key or $TAURI_SIGNING_PRIVATE_KEY, its password in
# $TAURI_SIGNING_PRIVATE_KEY_PASSWORD). Needs: curl, node with the repo's packages (the signer is
# @tauri-apps/cli's), Windows' own tar.exe (it makes and reads zips).
set -euo pipefail

repo="meltingice1337/ferret"
tag="ocr-pack-1"
name="ferret-ocr-pack-1.zip"
cd "$(dirname "$0")/.."
root=$PWD

cache="" out="$root/ferret-gpui/target/ocr-pack"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --cache) cache="${2:-}"; shift 2 ;;
    --out) out="${2:-}"; shift 2 ;;
    -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $1 (see --help)" >&2; exit 1 ;;
  esac
done

if [[ -t 1 ]]; then
  bold=$'\e[1m' dim=$'\e[2m' red=$'\e[31m' green=$'\e[32m' cyan=$'\e[36m' reset=$'\e[0m'
else
  bold="" dim="" red="" green="" cyan="" reset=""
fi
fail() { echo "${red}error:${reset} $*" >&2; exit 1; }
step() { echo "${cyan}==>${reset} $*"; }

# Windows' tar (bsdtar) reads and writes zips; Git Bash's own tar doesn't.
tar_exe="${SYSTEMROOT:-/c/Windows}/System32/tar.exe"
[[ -x "$tar_exe" ]] || tar_exe="/c/Windows/System32/tar.exe"
[[ -x "$tar_exe" ]] || fail "Windows' tar.exe wasn't found"
command -v curl >/dev/null || fail "curl wasn't found"
signer="$root/node_modules/@tauri-apps/cli/tauri.js"
[[ -f "$signer" ]] || fail "the signer isn't installed (run yarn): $signer"
win() { cygpath -w "$1"; }

# ---- the pinned sources ----
ort_version="1.31.1"
ort_name="onnxruntime-win-x64-$ort_version"
ort_url="https://github.com/microsoft/onnxruntime/releases/download/v$ort_version/$ort_name.zip"
ort_zip_sha="5869e26bb25314375f2d5ef1877e3ea06b1543d6c710e6e4a1ec2f7177ab4b2c"
# Files from its zip: path in it, SHA-256, path in the pack.
ort_files=(
  "lib/onnxruntime.dll cb2fa518b18356617f6f9f460c012501da2819ec7e5c1d80bd8f5de20492db0c onnxruntime.dll"
  "lib/onnxruntime_providers_shared.dll 8cea20be64259950cda8643af0ed42c2a424400fc3bb9bb29f9a35cee6b3419f onnxruntime_providers_shared.dll"
  "LICENSE c250d6278f0b47a6439fb7592b08b58a55eb9f535aa49a1db63211c3f982b674 LICENSE-onnxruntime.txt"
  "ThirdPartyNotices.txt c53a76501ef60db6f865f20599f220761201ac4057683acefdab37d861b86622 ThirdPartyNotices-onnxruntime.txt"
)
hf="https://huggingface.co/PaddlePaddle"
# Hugging Face repo, revision, file, SHA-256, path in the pack.
models=(
  "PP-OCRv5_mobile_det_onnx e6f4fa85f00e168c862bc462aebca69eef9b3d3d inference.onnx a431985659dc921974177a95adcfbb90fd9e51989a5e04d70d0b75f597b6e61d det/inference.onnx"
  "latin_PP-OCRv5_mobile_rec_onnx 89d3a50e2c27e2e7cceeab0e944c25c807d5db4f inference.onnx 7888113072263cb471b93f66dd5e2ad70548dc526fa1ace760d0d973dd121498 rec/inference.onnx"
  "latin_PP-OCRv5_mobile_rec_onnx 89d3a50e2c27e2e7cceeab0e944c25c807d5db4f inference.yml 0bbe984570f597af3638e50bdf2e8276f3ab26a61966096538b3b0d1849f5c84 rec/inference.yml"
  "PP-LCNet_x1_0_doc_ori_onnx 7330ab7039123e46af2dc03154b9969aa412c61d inference.onnx af9a0a4f317ff0709ce752067807f819cb15d883f8ecad89f28df1c6ee2d9c92 ori/inference.onnx"
)

mkdir -p "$out"
out=$(cd "$out" && pwd)
[[ -n "$cache" ]] || cache="$out/downloads"
mkdir -p "$cache"
cache=$(cd "$cache" && pwd)

# check FILE SHA256: stops unless the file is exactly the pinned one.
check() {
  local got
  got=$(sha256sum "$1" | cut -d' ' -f1)
  [[ "$got" == "$2" ]] || fail "$1 isn't the pinned file (SHA-256 $got, expected $2)"
}
# fetch URL DEST
fetch() {
  echo "${dim}    downloading $1${reset}"
  mkdir -p "$(dirname "$2")"
  curl -fL --retry 3 --proto '=https' --proto-redir '=https' -o "$2.part" "$1" || { rm -f "$2.part"; fail "the download failed: $1"; }
  mv "$2.part" "$2"
}

stage="$out/stage"
rm -rf "$stage" && mkdir -p "$stage"

# ---- ONNX Runtime ----
step "ONNX Runtime $ort_version"
ort_dir=""
for d in "$cache/$ort_name" "$cache/ort/$ort_name"; do
  [[ -f "$d/lib/onnxruntime.dll" ]] && { ort_dir=$d; echo "${dim}    unpacked, in $d${reset}"; break; }
done
if [[ -z "$ort_dir" ]]; then
  ort_zip="$cache/$ort_name.zip"
  [[ -f "$ort_zip" ]] || fetch "$ort_url" "$ort_zip"
  check "$ort_zip" "$ort_zip_sha"
  rm -rf "$out/ort" && mkdir -p "$out/ort"
  "$tar_exe" -x -f "$(win "$ort_zip")" -C "$(win "$out/ort")" || fail "$ort_zip couldn't be unpacked"
  ort_dir="$out/ort/$ort_name"
fi
for f in "${ort_files[@]}"; do
  read -r from sha to <<<"$f"
  check "$ort_dir/$from" "$sha"
  mkdir -p "$(dirname "$stage/$to")"
  cp "$ort_dir/$from" "$stage/$to"
done

# ---- the models ----
step "PaddleOCR PP-OCRv5 models"
for m in "${models[@]}"; do
  read -r hf_repo rev file sha to <<<"$m"
  src=""
  for c in "$cache/models/$hf_repo/$file" "$cache/$hf_repo/$file"; do
    [[ -f "$c" ]] && { src=$c; break; }
  done
  if [[ -z "$src" ]]; then
    src="$cache/models/$hf_repo/$file"
    fetch "$hf/$hf_repo/resolve/$rev/$file" "$src"
  fi
  check "$src" "$sha"
  echo "${dim}    $to: $hf_repo@${rev:0:10} $file${reset}"
  mkdir -p "$(dirname "$stage/$to")"
  cp "$src" "$stage/$to"
done

# ---- the Apache License's text (Apache-2.0 §4(a): copies of the models come with it) ----
step "Apache License 2.0 text"
apache="$cache/LICENSE-Apache-2.0.txt"
[[ -f "$apache" ]] || fetch "https://www.apache.org/licenses/LICENSE-2.0.txt" "$apache"
grep -q "Apache License" "$apache" && grep -q "Version 2.0, January 2004" "$apache" && grep -q "END OF TERMS AND CONDITIONS" "$apache" \
  || fail "$apache isn't the Apache License 2.0 text"
cp "$apache" "$stage/LICENSE-Apache-2.0.txt"

# ---- notice, manifest ----
model_line() {
  local hf_repo rev
  read -r hf_repo rev _ <<<"$1"
  echo "  $hf/$hf_repo"
  echo "  revision $rev"
}
cat >"$stage/NOTICE.txt" <<EOF
Ferret: better text recognition add-on (pack 1)

This pack holds other people's work, unchanged.

The text recognition models are PaddlePaddle's PP-OCRv5 (PaddleOCR), licensed under the Apache
License, Version 2.0 (its text: LICENSE-Apache-2.0.txt; https://www.apache.org/licenses/LICENSE-2.0),
from Hugging Face:

det/inference.onnx (text detection), PP-OCRv5_mobile_det_onnx:
$(model_line "${models[0]}")

rec/inference.onnx and rec/inference.yml (text recognition, Latin scripts), latin_PP-OCRv5_mobile_rec_onnx:
$(model_line "${models[1]}")

ori/inference.onnx (page orientation), PP-LCNet_x1_0_doc_ori_onnx:
$(model_line "${models[3]}")

onnxruntime.dll and onnxruntime_providers_shared.dll are Microsoft's ONNX Runtime $ort_version,
licensed under the MIT License (LICENSE-onnxruntime.txt; the notices of the software it includes
are in ThirdPartyNotices-onnxruntime.txt), from
  $ort_url
EOF

files=()
while IFS= read -r f; do files+=("$f"); done < <(cd "$stage" && find . -type f ! -name manifest.json | sed 's|^\./||' | LC_ALL=C sort)
{
  printf '{"pack":1,"files":{'
  sep=""
  for f in "${files[@]}"; do
    printf '%s"%s":"%s"' "$sep" "$f" "$(sha256sum "$stage/$f" | cut -d' ' -f1)"
    sep=","
  done
  printf '}}\n'
} >"$stage/manifest.json"

# ---- zip ----
step "Packing $name"
zip="$out/$name"
rm -f "$zip" "$zip.sig"
(cd "$stage" && "$tar_exe" -a -c -f "$(win "$zip")" manifest.json "${files[@]}") || fail "the zip couldn't be made"
listed=$("$tar_exe" -t -f "$(win "$zip")" | tr -d '\r' | LC_ALL=C sort)
wanted=$(printf '%s\n' manifest.json "${files[@]}" | LC_ALL=C sort)
[[ "$listed" == "$wanted" ]] || fail "the zip doesn't hold exactly the pack's files:"$'\n'"$listed"

# ---- sign (as scripts/release.sh does) ----
step "Signing it"
if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
  key="$HOME/.tauri/ferret.key"
  [[ -f "$key" ]] || fail "signing key not found at $key"
  export TAURI_SIGNING_PRIVATE_KEY="$key"
fi
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"
sign() {
  local args=(signer sign)
  if [[ -f "$TAURI_SIGNING_PRIVATE_KEY" ]]; then
    args+=(--private-key-path "$TAURI_SIGNING_PRIVATE_KEY")
    env -u TAURI_SIGNING_PRIVATE_KEY node "$signer" "${args[@]}" "$1" >/dev/null
  else
    node "$signer" "${args[@]}" "$1" >/dev/null
  fi
  [[ -s "$1.sig" ]] || fail "signing $1 failed"
}
sign "$zip"

size=$(stat -c %s "$zip")
unpacked=$(du -sb --exclude=manifest.json "$stage" | cut -f1)
echo
echo "${green}Built $name${reset}  $((size / 1024 / 1024)) MB zipped, $((unpacked / 1024 / 1024)) MB unpacked, ${#files[@]} files + manifest.json"
echo "  $zip"
echo "  $zip.sig"
echo "  sha256 $(sha256sum "$zip" | cut -d' ' -f1)"
echo
echo "To publish it (${bold}not run${reset}; --latest=false keeps Ferret's own latest release, the updater's feed, as it is):"
echo
printf '  gh release create %s "%s" "%s" -R %s --latest=false --title "Text recognition add-on (pack 1)" --notes "%s"\n' \
  "$tag" "$zip" "$zip.sig" "$repo" \
  "Better text recognition for scanned PDFs, downloaded by Ferret when asked (Settings → Add-ons): PaddleOCR PP-OCRv5 models (Apache-2.0) and ONNX Runtime $ort_version (MIT). See NOTICE.txt inside."
echo
echo "Ferret downloads it from https://github.com/$repo/releases/download/$tag/$name"
