# 進捗

> 何が終わって、何が進行中で、次に何をやるか。**このファイルは常にスリムに保つ。**
> 範囲は REQUIREMENTS.md、学び/罠は NOTES.md、詳細ログは docs/checkpoints/。

最終更新: 2026-09-04

## 現在のフェーズ
**v1 の要素が全部揃った。**録音 → 文字起こし → 時刻ベースの統合 → 要約まで通る。
残っているのは**実際の会議で通すこと**と、実運用で育てる部分（語彙辞書・要約の書式）。

## 次にやること (Top 3)
0. **業務ノートで `git fetch && git reset --hard origin/main`**（直近の一手）— 履歴を
   書き換えたので通常の `git pull` では追従できない。**通常の pull は失敗する。**
   そのうえでビルドが通るか確認する— `ctrlc` を足したので、
   GNU ツールチェーン側で `raw-dylib` の罠を踏む可能性がある。通らなければ開発デスクトップで
   作った単一 exe を運ぶ（`proto/dual-capture/README.md`「単一 exe として配る」）
1. **実際の会議で通す** — 秒数を省けば Ctrl+C / Enter まで録り続ける形になった。
   Teams の「通信デバイス」問題（未検証）もここで出る。**録音音量の確認を録る前に**
2. **whisper.cpp 内蔵の VAD を評価する** — `--vad` が CLI にある。文字起こし時間の半減と、
   無音区間の幻聴の抑制が狙い。非常口（ノート単体で一晩）を成立させるのもこれ
3. **業務ノート向けのインストール形式を作る**（ADR-011）— 依存ゼロの単一 exe は確認済み。
   トレイ常駐とホットキーが要る。**実会議を数回通してから**着手する

> 全体の流れとどの機械に何を置くかは **[README.md](README.md)**。
> 録音の手順・見る数字は **`proto/dual-capture/README.md`**。
> 実機の構成は REQUIREMENTS.md「実機の構成」。意思決定は **[docs/adr.md](docs/adr.md)**（11件）。

## 未検証で残っているもの
- **Teams の「通信デバイス」問題**（NOTES.md リスク1）。実際の会議でしか出ない。次の会議で回す。
- **非常口に `claude` CLI が要る。** `summarize` は `claude -p` を呼ぶ。業務ノートに
  Claude Code が入っているか未確認。無ければ、非常口は文字起こしと統合までになる。

## 完了
- [x] 2026-09-03 初期構成 → [checkpoint](docs/checkpoints/2026-09-03.md)
- [x] 2026-09-03 ループバック録音を実証、業務ノートで実測、proto を逐次書き出しへ改修
      → [checkpoint](docs/checkpoints/2026-09-03.md)
- [x] 2026-09-04 ヘッドセットで再計測、ダウンミックスのバグ修正
      → [checkpoint](docs/checkpoints/2026-09-04.md)
- [x] 2026-09-04 70分の連続録音。ドリフトの否定と録音側の前提確定
      → [checkpoint](docs/checkpoints/2026-09-04.md)
- [x] 2026-09-04 文字起こしベンチ。業務ノートの CPU では33倍足りないことを確定
      → [checkpoint](docs/checkpoints/2026-09-04.md)
- [x] 2026-09-04 時刻ベースの統合器 `proto/merge-transcript` を実装（回帰テスト付き）
      → [checkpoint](docs/checkpoints/2026-09-04.md)
- [x] 2026-09-04 デスクトップに whisper.cpp を導入。録音→文字起こし→統合を実データで検証
      → [checkpoint](docs/checkpoints/2026-09-04.md)
- [x] 2026-09-04 要約 `proto/summarize` を実装。**v1 の要素が揃った**
      → [checkpoint](docs/checkpoints/2026-09-04.md)

## 進行中
(なし)

## ADR（意思決定の記録）
9件。**[docs/adr.md](docs/adr.md)** に移した（このファイルの行数上限に触れたため）。
