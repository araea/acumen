#!/data/data/com.termux/files/usr/bin/bash
# 发布一个版本（维护者用，在 Termux 本机上跑）：
#
#   scripts/release.sh            构建、打包，发到 GitHub Releases（标签 v<版本>）
#   scripts/release.sh --dry-run  只构建打包到 dist/，不发布
#
# 版本号取 Cargo.toml。说明取 CHANGELOG.md 里这个版本那一节。
# 发布物：acumen-termux-aarch64.gz（可执行文件）、update.json（版本、下载地址、校验和与签名；
# 已装的实例每天例行维护时读它）、install.sh（一行安装命令拉的就是它）。
#
# acumen-termux-aarch64.gz 同时是离线安装包：签名写在 gzip 头的注释里。另存一份
# acumen-<版本>.gz 到手机的下载目录，经 QQ / 微信发给连不上 GitHub 的人，对方点开下载完、
# 在 Termux 里 acumen update 就换上了（知微会自己去 QQ、微信的接收目录里找）。
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
tag=v$version
asset=acumen-termux-aarch64.gz

cargo build --release --locked
rm -rf dist && mkdir dist
cp scripts/install.sh dist/install.sh
sha=$(sha256sum target/release/acumen | cut -d' ' -f1)

notes=$(awk -v v="$version" '
    /^## / { on = ($2 == v) ; next }
    on { print }
' CHANGELOG.md | sed '/./,$!d')
[[ -n $notes ]] || { echo "CHANGELOG.md 里没有 $version 这一节" >&2; exit 1; }
summary=$(printf '%s\n' "$notes" | sed -n 's/^- //p' | head -n 3 | paste -sd '；' -)

# 清单签名：已装的实例会经第三方加速站取清单和可执行文件，只认这把私钥签过的。
# 私钥丢了，老版本就再也认不出新清单（只能重装），务必备份。
key=${ACUMEN_SIGNING_KEY:-$HOME/.config/acumen/release-signing-key.pem}
[[ -r $key ]] || { echo "没有签名私钥 $key" >&2; exit 1; }
printf 'acumen %s %s' "$version" "$sha" > dist/.signed
signature=$(openssl pkeyutl -sign -rawin -inkey "$key" -in dist/.signed | base64 -w0)
rm -f dist/.signed

# 可执行文件压成 gzip，头里带上注释「acumen 版本 校验和 签名」（FCOMMENT）：gzip -dc
# 照常解压，知微拿它认本地安装包。gzip -n 的头是固定 10 字节、标志位为 0，在后面插注释即可。
gzip -9 -n -c target/release/acumen > dist/.raw.gz
{
    printf '\x1f\x8b\x08\x10'
    head -c 10 dist/.raw.gz | tail -c 6
    printf '%s\0' "acumen $version $sha $signature"
    tail -c +11 dist/.raw.gz
} > "dist/$asset"
rm -f dist/.raw.gz
gzip -t "dist/$asset"

# 离线安装包另存到手机的下载目录，方便直接发给别人；旧版本的顺手删掉。
share=/storage/emulated/0/Download
if [[ -d $share && -w $share ]]; then
    find "$share" -maxdepth 1 -name 'acumen-*.gz' ! -name "acumen-$version.gz" -delete
    cp "dist/$asset" "$share/acumen-$version.gz"
    echo "离线安装包：$share/acumen-$version.gz"
else
    echo "下载目录写不进去（termux-setup-storage 授权存储），离线安装包只在 dist/$asset" >&2
fi

jq -n --arg version "$version" --arg sha "$sha" --arg notes "$summary" --arg signature "$signature" \
    --arg url "https://github.com/araea/acumen/releases/download/$tag/$asset" \
    '{version: $version, url: $url, sha256: $sha, signature: $signature, notes: $notes}' > dist/update.json
ls -la dist

if [[ ${1:-} == --dry-run ]]; then exit 0; fi
git push origin HEAD
# 经代理连 api.github.com 偶尔会断（unexpected EOF），断了就清掉半截的再来。
for attempt in 1 2 3; do
    gh release create "$tag" dist/* --target "$(git rev-parse HEAD)" --title "$tag" --notes "$notes" && exit 0
    gh release delete "$tag" --yes >/dev/null 2>&1
    sleep 5
done
exit 1
