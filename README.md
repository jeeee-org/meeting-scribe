# meeting-scribe

Slack / Teams の打ち合わせを録り、**話者ラベル付きのトランスクリプトと要約テキスト**を作る。

自分の声（マイク）と相手の声（PC の再生音）を**別トラックで**録るのが本体価値。
混ぜて1本にすると誰の発言か落ちる。

開発デスクトップはこのリポジトリを pull して `cargo run`。
**業務ノートへは、いずれインストール形式で配る**（ADR-011。依存ゼロの単一 exe になることは
確認済み。着手は実会議を数回通してから）。いまは業務ノートも pull して使っている。

---

## 流れ

```
[業務ノート]                    [開発デスクトップ]
 dual-capture                    whisper.cpp ──→ merge-transcript ──→ summarize
   ├─ *-self.wav      ────→        文字起こし        時刻で統合          要約
   ├─ *-other.wav     ────→        (2本)          *-transcript.txt   *-summary.txt
   └─ *-meta.json ×2  ────→
        受け渡し: USB SSD か同一 LAN 直結（ADR-008。クラウドは経由しない）
```

**`*-meta.json` を一緒に運ぶこと。** WAV だけ持ってきても統合できない。
WAV の中の時刻は実時刻ではなく、meta.json の `drops_sec_len` が変換の鍵になる（ADR-004）。

## どの機械に何を置くか

| | 業務ノート（録音） | 開発デスクトップ（処理） |
|---|---|---|
| `proto/dual-capture` | **ここだけで使う**（Windows 側） | — |
| whisper.cpp | 非常口用に CPU 版を導入済み | **cuBLAS 版（本番）** |
| `proto/merge-transcript` | 非常口のときだけ | **本番**（WSL で動く） |
| `proto/summarize` | 非常口のときだけ | **本番**（WSL で動く） |

**業務ノートに増やすものはもう無い。**本番経路ではモデルも whisper も要らない。

### 非常口（音声を持ち出せない日）
業務ノート単体で一晩かける経路を残してある（ADR-008 / ADR-009。要約は「その日のうち」で足りる）。
文字起こしは実時間比 3.353 なので60分の会議に3.4時間、2トラックで7.8時間かかる。
**VAD を入れれば半減して一晩に収まる**（未実装）。

**未確認**: `summarize` は `claude` CLI を呼ぶので、業務ノートに Claude Code が要る。
入っていなければ、文字起こしと統合までをノートで行い、テキストだけ持ち出す形になる。

## 使い方

### 1. 録音（業務ノート・Windows 側）
```
proto\dual-capture\run.cmd --mic=Logi --loopback=Logi
```
**秒数は指定しない。**会議は長さが読めないので、Ctrl+C か Enter で止まるまで録り続ける。
**強制終了させないこと。**サイズ欄が書かれず WAV が読めなくなる（`--repair` で戻せるが
時刻の補正を失う）。

**録音前に Windows の録音音量を確認する。**デバイスごとに別設定で、余裕は 2.2 dB しかない。
詳細は [proto/dual-capture/README.md](proto/dual-capture/README.md)。

### 2. 文字起こし（開発デスクトップ）
```
whisper-cli -m ggml-large-v3-turbo-q5_0.bin -l ja -oj -f dual-<epoch>-self.wav
whisper-cli -m ggml-large-v3-turbo-q5_0.bin -l ja -oj -f dual-<epoch>-other.wav
```
実時間比 0.0459。69分の音声で3.2分、2トラックで6.3分。
**初回だけ10秒ほど余分にかかる**（PTX の JIT。以降はキャッシュされる）。

### 3. 統合（WSL で可）
```
cargo run --release --manifest-path proto/merge-transcript/Cargo.toml -- dual-<epoch>
```
→ `dual-<epoch>-transcript.txt`。詳細は [proto/merge-transcript/README.md](proto/merge-transcript/README.md)。

### 4. 要約（WSL で可）
```
cargo run --release --manifest-path proto/summarize/Cargo.toml -- dual-<epoch> --dry-run
cargo run --release --manifest-path proto/summarize/Cargo.toml -- dual-<epoch>
```
→ `dual-<epoch>-summary.txt`。詳細は [proto/summarize/README.md](proto/summarize/README.md)。

## 会議の中身の扱い

- **録音・文字起こし・要約はリポジトリに入れない**（`.gitignore` 済み）。作業先はリポジトリ外。
- **外部送信は要約の1箇所だけ。**録音も文字起こしもローカルで完結している。
  `summarize --dry-run` で送る内容を送信前に確認できる。
- **`ANTHROPIC_API_KEY` を環境変数に置かない**（CLAUDE.md）。要約はサブスクの Claude で動く。

## 記録の置き場

| ファイル | 中身 |
|---|---|
| [REQUIREMENTS.md](REQUIREMENTS.md) | 仕様・スコープ・実機の構成・未決事項 |
| [PROGRESS.md](PROGRESS.md) | 現在地と次にやること |
| [docs/adr.md](docs/adr.md) | 意思決定の記録（10件） |
| [NOTES.md](NOTES.md) | 学び・罠・実測値 |
| [docs/checkpoints/](docs/checkpoints/) | 日ごとの詳細ログ |
