# 進捗

> 何が終わって、何が進行中で、次に何をやるか。**このファイルは常にスリムに保つ。**
> 範囲は REQUIREMENTS.md、学び/罠は NOTES.md、詳細ログは docs/checkpoints/。

最終更新: 2026-09-04

## 現在のフェーズ
**録音と統合まで通った。**2トラックを実時刻で突き合わせ、話者ラベル付きトランスクリプトを
出すところまで書けた（`proto/merge-transcript`）。残るのは**文字起こしの実行環境と要約**。

## 次にやること (Top 3)
1. **開発デスクトップに whisper.cpp の cuBLAS ビルド済みを入れる** — ビルド不要。
   `bench-5min.wav` で同一入力の実測を取り、実時間比 0.0736 の見込みを確かめる
2. **要約を書く** — 統合済みトランスクリプトを入力に、決まったこと / ToDo / 持ち越しを出す。
   実行手段は未決（Anthropic API かサブスクの Claude か）
3. **VAD（無音カット）を入れる** — 文字起こし時間を半減でき、無音区間の幻聴も減る。
   非常口（ノート単体で一晩）を成立させるのもこれ（ADR-008）

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

## 進行中
(なし)

## ADR（意思決定の記録）
9件。**[docs/adr.md](docs/adr.md)** に移した（このファイルの行数上限に触れたため）。
