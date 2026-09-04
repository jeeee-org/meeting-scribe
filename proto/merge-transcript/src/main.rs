//! 2トラックの文字起こしを、時刻で突き合わせて1本のトランスクリプトにする。
//!
//! **なぜ単純に並べられないか。** WAV の中の時刻は実時刻ではない。WASAPI ループバックは
//! 再生中のアプリが無いとパケットを返さないことがあり、相手が黙っていた分だけ WAV の
//! 時間軸が詰まる（ADR-004）。マイク側も起動遅延と underrun のぶんだけ詰まる（ADR-006）。
//! whisper が返すのは **WAV 内の時刻**なので、そのまま並べると2トラックがずれる。
//!
//! `dual-capture` が `*-meta.json` に残した `drops_sec_len`（実時刻, 空いた秒数）から、
//! WAV 内時刻 → 実時刻 の変換を復元する:
//!
//!   累積ラグ  L_i = Σ len          （i 番目までの空白の合計）
//!   段差の位置 W_i = at_i − L_i     （その空白が WAV 上のどこで立つか）
//!   実時刻    t_real = t_wav + L(t_wav)   ただし L(t) = W_i ≤ t を満たす最後の L_i
//!
//! 先頭の空白も `drops_sec_len` に入っているので、**この配列だけで復元できる**
//! （`first_data_sec` は参考値。2つの原点を混ぜない）。
//!
//! 使い方:
//!   merge-transcript <stem>            例: merge-transcript dual-1788502024
//!   merge-transcript <stem> --debug    WAV 内時刻と補正量も出す
//!
//! `<stem>-self-meta.json` / `<stem>-self.wav.json` の4ファイルを読む。
//! whisper の JSON は `whisper-cli -oj` の出力（`transcription[].offsets`）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

/// 同じ話者の発話をまとめる間隔。これより短く続いたら1行にする。
/// whisper は文単位で切るので、そのまま出すと短い行の壁になって読みにくい。
const MERGE_GAP: f64 = 2.0;

struct Track {
    speaker: &'static str,
    device: String,
    rate: u64,
    /// (WAV 内時刻, その位置までの累積ラグ)。時刻の変換表。
    steps: Vec<(f64, f64)>,
    total_lag: f64,
}

struct Seg {
    start: f64,
    end: f64,
    speaker: &'static str,
    text: String,
    /// 補正前（WAV 内）の開始時刻。--debug でのみ使う。
    wav_start: f64,
}

fn read_json(path: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("{} を読めない", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("{} が JSON として読めない", path.display()))
}

/// `drops_sec_len` から時刻の変換表を作る。
///
/// 返すのは (WAV 内の段差位置, その位置までの累積ラグ) と、総ラグ。
/// 先頭の空白は `at == len` なので位置 0 に立つ。
fn steps_from_drops(drops: &[(f64, f64)]) -> (Vec<(f64, f64)>, f64) {
    let mut steps = Vec::new();
    let mut acc = 0.0f64;
    for (at, len) in drops {
        acc += len;
        steps.push(((at - acc).max(0.0), acc));
    }
    steps.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    (steps, acc)
}

/// WAV 内時刻 `t_wav` に効く累積ラグ。
///
/// 段差の位置は `at − 累積ラグ` という引き算で出すので **丸め誤差が乗る**。
/// 実際 `10.500 − 9.800 = 0.7000000000000002` になり、境界ちょうどのセグメント
/// （whisper の刻みは 10ms なので境界に乗りやすい）が段差を1つ飛ばして、
/// 9.6秒ずれたまま出力されていた。**静かに壊れる型**なので許容差を置く。
fn lag_at(steps: &[(f64, f64)], t_wav: f64) -> f64 {
    // 1マイクロ秒。whisper の刻み（10ms）よりはるかに細かく、意味のある差は潰さない。
    const EPS: f64 = 1e-6;
    let mut lag = 0.0;
    for (pos, acc) in steps {
        if *pos <= t_wav + EPS {
            lag = *acc;
        } else {
            break;
        }
    }
    lag
}

impl Track {
    fn from_meta(path: &Path, speaker: &'static str) -> Result<Self> {
        let v = read_json(path)?;
        let rate = v["sample_rate"]
            .as_u64()
            .ok_or_else(|| anyhow!("{}: sample_rate が無い", path.display()))?;
        let device = v["device"].as_str().unwrap_or("(不明)").to_string();

        let mut drops: Vec<(f64, f64)> = Vec::new();
        if let Some(arr) = v["drops_sec_len"].as_array() {
            for d in arr {
                drops.push((d[0].as_f64().unwrap_or(0.0), d[1].as_f64().unwrap_or(0.0)));
            }
        }
        let (steps, total_lag) = steps_from_drops(&drops);
        Ok(Self { speaker, device, rate, steps, total_lag })
    }

    /// WAV 内の時刻を実時刻へ直す。
    ///
    /// 段差の位置は `at − 累積ラグ` という引き算で出すので、**丸め誤差が乗る**。
    /// 実際 `10.500 − 9.800 = 0.7000000000000002` になり、境界ちょうどのセグメント
    /// （whisper の刻みは 10ms なので境界に乗りやすい）が段差を1つ飛ばして、
    /// 9.6秒ずれたまま出力されていた。**静かに壊れる型**なので許容差を置く。
    fn to_real(&self, t_wav: f64) -> f64 {
        t_wav + lag_at(&self.steps, t_wav)
    }

    /// whisper-cli -oj の出力からセグメントを読み、実時刻に直して返す。
    fn segments(&self, path: &Path) -> Result<Vec<Seg>> {
        let v = read_json(path)?;
        let items = v["transcription"]
            .as_array()
            .ok_or_else(|| anyhow!("{}: transcription が無い。whisper-cli -oj の出力か確認する", path.display()))?;

        let mut out = Vec::new();
        for item in items {
            // offsets はミリ秒。timestamps は "00:00:07,000" 形式で、こちらは予備。
            let (from_ms, to_ms) = match (item["offsets"]["from"].as_f64(), item["offsets"]["to"].as_f64()) {
                (Some(f), Some(t)) => (f, t),
                _ => {
                    let f = parse_timestamp(item["timestamps"]["from"].as_str().unwrap_or(""));
                    let t = parse_timestamp(item["timestamps"]["to"].as_str().unwrap_or(""));
                    match (f, t) {
                        (Some(f), Some(t)) => (f * 1000.0, t * 1000.0),
                        _ => bail!("{}: セグメントに offsets も timestamps も無い", path.display()),
                    }
                }
            };
            let text = item["text"].as_str().unwrap_or("").trim().to_string();
            if text.is_empty() {
                continue;
            }
            let wav_start = from_ms / 1000.0;
            out.push(Seg {
                start: self.to_real(wav_start),
                end: self.to_real(to_ms / 1000.0),
                speaker: self.speaker,
                text,
                wav_start,
            });
        }
        Ok(out)
    }
}

/// "00:01:07,860" / "00:01:07.860" を秒へ。
fn parse_timestamp(s: &str) -> Option<f64> {
    let s = s.replace(',', ".");
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let h: f64 = parts[0].parse().ok()?;
    let m: f64 = parts[1].parse().ok()?;
    let sec: f64 = parts[2].parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + sec)
}

fn hhmmss(t: f64) -> String {
    let t = t.max(0.0);
    let h = (t / 3600.0) as u64;
    let m = ((t % 3600.0) / 60.0) as u64;
    let s = t % 60.0;
    if h > 0 {
        format!("{h}:{m:02}:{s:04.1}")
    } else {
        format!("{m:02}:{s:04.1}")
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let debug = args.iter().any(|a| a == "--debug");
    let stem = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .ok_or_else(|| anyhow!("使い方: merge-transcript <stem> [--debug]\n  例: merge-transcript dual-1788502024"))?;

    let p = |suffix: &str| -> PathBuf { PathBuf::from(format!("{stem}{suffix}")) };

    let mut tracks = Vec::new();
    let mut segs: Vec<Seg> = Vec::new();
    for (suffix, speaker) in [("-self", "自分"), ("-other", "相手")] {
        let meta = p(&format!("{suffix}-meta.json"));
        let asr = p(&format!("{suffix}.wav.json"));
        if !meta.exists() {
            eprintln!("  !! {} が無い。この系統は飛ばす。", meta.display());
            continue;
        }
        let track = Track::from_meta(&meta, speaker)?;
        if !asr.exists() {
            eprintln!(
                "  !! {} が無い。whisper-cli -oj を通してから実行する。",
                asr.display()
            );
            continue;
        }
        segs.extend(track.segments(&asr)?);
        tracks.push(track);
    }
    if tracks.is_empty() {
        bail!("読めるトラックが無い。stem が正しいか確認する: {stem}");
    }

    // 開始時刻で並べる。同時刻なら相手を先に置く（こちらが相槌を打つ形が自然に読める）。
    segs.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap()
            .then(b.speaker.cmp(a.speaker))
    });

    let mut out = String::new();
    out.push_str("会議トランスクリプト\n");
    out.push_str(&format!("録音: {stem}\n"));
    for t in &tracks {
        out.push_str(&format!(
            "{}: {} / {} Hz / 時刻補正 +{:.2}秒（空白 {} 箇所）\n",
            t.speaker,
            t.device,
            t.rate,
            t.total_lag,
            t.steps.len()
        ));
    }
    out.push_str("時刻は録音開始からの実時刻。WAV 内の時刻ではない（ADR-004）。\n\n");

    // 同じ話者が続く短い間隔はまとめる。whisper は文単位で切るので、
    // そのまま出すと短い行の壁になって読みにくく、要約にも効かない。
    let mut i = 0;
    while i < segs.len() {
        let start = segs[i].start;
        let speaker = segs[i].speaker;
        let wav_start = segs[i].wav_start;
        let mut text = segs[i].text.clone();
        let mut end = segs[i].end;
        let mut j = i + 1;
        while j < segs.len() && segs[j].speaker == speaker && segs[j].start - end <= MERGE_GAP {
            text.push(' ');
            text.push_str(&segs[j].text);
            end = segs[j].end;
            j += 1;
        }
        if debug {
            out.push_str(&format!(
                "[{}] {}: {}\n        （WAV内 {} / 補正 +{:.2}秒）\n",
                hhmmss(start),
                speaker,
                text,
                hhmmss(wav_start),
                start - wav_start
            ));
        } else {
            out.push_str(&format!("[{}] {}: {}\n", hhmmss(start), speaker, text));
        }
        i = j;
    }

    let dest = format!("{stem}-transcript.txt");
    std::fs::write(&dest, &out).with_context(|| format!("{dest} を書けない"))?;

    // 発話量の内訳。どちらが喋っていたかは要約の質を見るときの手がかりになる。
    let mut talk: BTreeMap<&str, f64> = BTreeMap::new();
    for s in &segs {
        *talk.entry(s.speaker).or_insert(0.0) += s.end - s.start;
    }
    println!("{dest} を書いた（{} セグメント）", segs.len());
    for (sp, sec) in talk {
        println!("  {sp}: 発話 {:.1} 分", sec / 60.0);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 実測から採った形: 先頭 0.2 秒の空白 + 実時刻 10.5 秒に 9.6 秒の穴。
    fn sample() -> Vec<(f64, f64)> {
        vec![(0.200, 0.200), (10.500, 9.600)]
    }

    #[test]
    fn steps_positions() {
        let (steps, total) = steps_from_drops(&sample());
        assert_eq!(steps.len(), 2);
        assert!(steps[0].0.abs() < 1e-9, "先頭の空白は位置 0 に立つ");
        assert!((steps[1].0 - 0.7).abs() < 1e-9);
        assert!((total - 9.8).abs() < 1e-9);
    }

    /// 回帰テスト: 境界ちょうどで段差を飛ばしていた。
    /// `10.500 - 9.800` は 0.7 ぴったりにならないので、素の `<=` だと偽になる。
    #[test]
    fn boundary_segment_lands_after_the_gap() {
        let (steps, _) = steps_from_drops(&sample());
        assert!((lag_at(&steps, 0.7) - 9.8).abs() < 1e-9, "9.6秒ずれる原因だった箇所");
        assert!((lag_at(&steps, 0.69) - 0.2).abs() < 1e-9, "段差の手前は手前のまま");
        assert!((lag_at(&steps, 8.0) - 9.8).abs() < 1e-9);
    }

    #[test]
    fn leading_gap_only_is_a_constant_offset() {
        let (steps, _) = steps_from_drops(&[(0.250, 0.250)]);
        assert!((lag_at(&steps, 0.0) - 0.25).abs() < 1e-9);
        assert!((lag_at(&steps, 3600.0) - 0.25).abs() < 1e-9);
    }

    /// ゼロ埋めされた実行では drops が空になる。補正なしで素通し。
    #[test]
    fn no_gaps_means_no_correction() {
        let (steps, total) = steps_from_drops(&[]);
        assert!(lag_at(&steps, 123.4).abs() < 1e-9);
        assert_eq!(total, 0.0);
    }
}
