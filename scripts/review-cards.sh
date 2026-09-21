#!/usr/bin/env bash
# 本地合成样张；不连接 QQ，不调用模型，也不发送消息。
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
export CARD_ARTIFACTS="${CARD_ARTIFACTS:-${TMPDIR:-/tmp}/acumen-cards}"
export HELP_CARD_DUMP="$CARD_ARTIFACTS/help"
export CTL_CARD_DUMP="$CARD_ARTIFACTS/ctl"
export AI_NEWS_CARD_DUMP="$CARD_ARTIFACTS/ai_news"
export OAI_CARD_DUMP="$CARD_ARTIFACTS/oai"
export WORDCLOUD_CARD_DUMP="$CARD_ARTIFACTS/wordcloud"
export STATS_CARD_DUMP="$CARD_ARTIFACTS/stats"
mkdir -p "$STATS_CARD_DUMP"
export ACUMEN_CHART_PREVIEW="$STATS_CARD_DUMP/types.png"
export ACUMEN_CHART_PREVIEW_BAR="$STATS_CARD_DUMP/ranking.png"
cargo test --locked renders_sample_cards_to_png -- --ignored --test-threads=1
cargo test --locked dump_sample_cards -- --ignored --test-threads=1
cargo test --locked dump_full_ranking_sample -- --ignored --test-threads=1
cargo test --locked stats::chart::renderer::tests -- --test-threads=1
node tests/cards.cjs
printf '样张与布局报告：%s/index.html\n' "$CARD_ARTIFACTS"
