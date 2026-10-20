#!/data/data/com.termux/files/usr/bin/bash
# 发布一个版本（维护者用，在 Termux 本机上跑）：
#
#   scripts/release.sh            构建、打包，发到 GitHub Releases（标签 v<版本>）
#   scripts/release.sh --dry-run  只构建打包到 dist/，不发布
#
# 版本号取 Cargo.toml。说明取 CHANGELOG.md 里这个版本那一节。
# 发布物：acumen-termux-aarch64.gz（可执行文件）、update.json（版本、下载地址、校验和；
# 已装的实例每天例行维护时读它）、install.sh（一行安装命令拉的就是它）。
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
tag=v$version
asset=acumen-termux-aarch64.gz

cargo build --release --locked
rm -rf dist && mkdir dist
gzip -9 -c target/release/acumen > "dist/$asset"
cp scripts/install.sh dist/install.sh
sha=$(sha256sum target/release/acumen | cut -d' ' -f1)

notes=$(awk -v v="$version" '
    /^## / { on = ($2 == v) ; next }
    on { print }
' CHANGELOG.md | sed '/./,$!d')
[[ -n $notes ]] || { echo "CHANGELOG.md 里没有 $version 这一节" >&2; exit 1; }
summary=$(printf '%s\n' "$notes" | sed -n 's/^- //p' | head -n 3 | paste -sd '；' -)

jq -n --arg version "$version" --arg sha "$sha" --arg notes "$summary" \
    --arg url "https://github.com/araea/acumen/releases/download/$tag/$asset" \
    '{version: $version, url: $url, sha256: $sha, notes: $notes}' > dist/update.json
ls -la dist

if [[ ${1:-} == --dry-run ]]; then exit 0; fi
git push origin HEAD
gh release create "$tag" dist/* --target "$(git rev-parse HEAD)" --title "$tag" --notes "$notes"
