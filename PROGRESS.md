# 進捗

> 何が終わって、何が進行中で、次に何をやるか。**このファイルは常にスリムに保つ。**
> 範囲は REQUIREMENTS.md、学び/罠は NOTES.md、詳細ログは docs/checkpoints/。

最終更新: 2026-09-04

## 現在のフェーズ
**録音 → 文字起こし → 統合が実データで通った。**開発デスクトップで実時間比 0.0459
（業務ノート CPU の73倍）。話者ラベル付きトランスクリプトまで出る。**残るのは要約だけ。**

## 次にやること (Top 3)
1. **要約を書く** — 統合済みトランスクリプトを入力に、決まったこと / ToDo / 持ち越しを出す。
   実行手段は未決（Anthropic API かサブスクの Claude か）。**ここが最後の未実装**
2. **whisper.cpp 内蔵の VAD を評価する** — `--vad` が CLI にある。自前で書く前にこれを試す。
   文字起こし時間の半減と、無音区間の幻聴の抑制が狙い
3. **実際の会議で通す** — Teams の「通信デバイス」問題（未検証）もここで出る

> 手順・見る数字・結果の渡し方は **`proto/dual-capture/README.md`**。
> 実機の構成は REQUIREMENTS.md「実機の構成」。意思決定は **[docs/adr.md](docs/adr.md)**。

## 未検証で残っているもの
- **Teams の「通信デバイス」問題**（NOTES.md リスク1）。実際の会議でしか出ない。次の会議で回す。

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

## 進行中
(なし)

## ADR（意思決定の記録）
9件。**[docs/adr.md](docs/adr.md)** に移した（このファイルの行数上限に触れたため）。
