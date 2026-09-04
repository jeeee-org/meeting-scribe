//! 統合済みトランスクリプトを要約する。
//!
//! 実行手段は **サブスクの Claude（`claude -p`）**。voice-input では
//! 「プロセス起動と巨大なシステムプロンプトで TTFT が持たない」として使わないと決めたが、
//! それはレイテンシ要件から出た判断で、このPJには効かない。停止後に数十秒かけてよいので、
//! 追加の従量課金がゼロという利点だけが残る（NOTES.md）。
//!
//! **ここがこのPJで唯一、会議の中身を外へ出す場所。** 録音も文字起こしもローカルで完結して
//! いるので、送信はこの1箇所に閉じている。だから何を送るかを送信前に確かめられるよう
//! `--dry-run` を最初から用意する。
//!
//! 使い方:
//!   summarize <stem>                 <stem>-transcript.txt → <stem>-summary.txt
//!   summarize <stem> --dry-run       送る内容を表示するだけ（送信しない）
//!   summarize <stem> --model opus    モデルを指定する（既定は claude の設定に従う）
//!
//! `ANTHROPIC_API_KEY` は使わない・設定しない（CLAUDE.md）。`claude` のサブスクで動く。

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};

/// 要約の指示。**プロンプトはソースに置いて版管理する。**
/// 要約の質はここで決まるので、実運用で育てる対象になる（REQUIREMENTS.md）。
const INSTRUCTIONS: &str = r#"あなたは打ち合わせの書記です。標準入力に会議のトランスクリプトが渡されます。

トランスクリプトの性質:
- マイク（自分）と PC の再生音（相手）を別々に録り、時刻で統合したものです。
- `自分:` は録音者本人、`相手:` はそれ以外の参加者です。相手が複数人でも区別されていません。
- 行頭の [分:秒.秒] または [時:分:秒.秒] は録音開始からの経過時刻です（例: [02:33.0]、[1:05:12.4]）。
- **音声認識の誤りが含まれます。** 固有名詞や専門用語が誤変換されていることがあります。
  文脈から明らかな誤変換は自然な表記に直して構いませんが、**判断がつかないものは原文のまま残し、
  推測で別の語に置き換えないでください。**

書き方の決まり:
- **トランスクリプトに書かれていないことは書かないでください。** 推測・補完・一般論は不要です。
  該当がない項目には「言及なし」とだけ書いてください。
- 重要な項目には、根拠となる発言の時刻を添えてください。**時刻はトランスクリプトの行頭に
  書かれている表記をそのまま写してください。形式を変換しないでください**（[02:33.0] とあれば
  [02:33.0] と書く）。トランスクリプトを検索して原文に戻るための目印なので、
  表記が変わると探せなくなります。
- ToDo は「誰が・何を・いつまでに」を書いてください。期限が話されていなければ「期限の言及なし」と
  書いてください。勝手に期限を作らないでください。
- **Markdown 記法を使わないでください。** アスタリスクによる強調、# 見出し、- 箇条書き、表は
  使いません。そのまま他所へ貼れる素のテキストにしてください。見出しは【】、箇条書きは・を使います。

次の形式で出力してください。前置きや後書きは不要です。

【一言サマリ】
この打ち合わせが何だったかを1〜2文で。

【決まったこと】
・（決定事項。時刻を添える）

【自分のToDo】
・（自分がやること。期限があれば添える）

【相手のToDo】
・（相手がやること。期限があれば添える）

【未解決の論点・次回への持ち越し】
・（結論が出なかったこと、保留になったこと）

【気になった点】
・（音声認識が怪しく、原文を確認したほうがよい箇所があれば時刻とともに。無ければ「なし」）
"#;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let model = args.iter().find_map(|a| a.strip_prefix("--model=").map(str::to_string));
    let stem = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .ok_or_else(|| {
            anyhow!("使い方: summarize <stem> [--dry-run] [--model=<名前>]\n  例: summarize dual-1788514161")
        })?;

    let src = format!("{stem}-transcript.txt");
    let transcript = std::fs::read_to_string(&src)
        .with_context(|| format!("{src} を読めない。先に merge-transcript を通す。"))?;
    if transcript.trim().is_empty() {
        bail!("{src} が空。文字起こしと統合を確認する。");
    }

    // 何を外へ出すのかを必ず表示する。ここがこのPJで唯一の外部送信。
    let lines = transcript.lines().count();
    let chars = transcript.chars().count();
    println!("入力  : {src}（{lines} 行 / {chars} 文字）");
    println!("送信先: Anthropic（サブスクの Claude / `claude -p`）");
    println!("        **会議の中身がここで外に出る。**録音と文字起こしはローカルで完結している。");
    if let Some(m) = &model {
        println!("モデル: {m}");
    }

    if dry_run {
        println!("\n--- 指示 ---\n{INSTRUCTIONS}");
        println!("--- 入力の冒頭 ---");
        for l in transcript.lines().take(15) {
            println!("{l}");
        }
        if lines > 15 {
            println!("…（残り {} 行）", lines - 15);
        }
        println!("\n--dry-run のため送信しなかった。");
        return Ok(());
    }

    let mut cmd = Command::new("claude");
    cmd.arg("-p").arg(INSTRUCTIONS);
    if let Some(m) = &model {
        cmd.arg("--model").arg(m);
    }
    // トランスクリプトは引数ではなく標準入力で渡す。1時間の会議で2万字規模になり、
    // 引数の長さ制限に当たりうるため。
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("`claude` を起動できない。Claude Code の CLI が PATH にあるか確認する。")?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("標準入力を開けない"))?
        .write_all(transcript.as_bytes())?;

    println!("\n要約中…（会議30分あたり十数秒かかる）");
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("claude が失敗した（終了コード {:?}）", out.status.code());
    }
    let summary = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if summary.is_empty() {
        bail!("要約が空で返った。`claude -p` が動くか単体で確かめる。");
    }

    let dest = format!("{stem}-summary.txt");
    std::fs::write(&dest, format!("{summary}\n"))
        .with_context(|| format!("{dest} を書けない"))?;
    println!("\n{dest} を書いた（{} 文字）\n", summary.chars().count());
    println!("{summary}");
    Ok(())
}
