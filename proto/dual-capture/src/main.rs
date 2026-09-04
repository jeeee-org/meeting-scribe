//! 2系統同時録音の計測プロトタイプ（Windows 専用）。
//!
//! マイク（自分）と WASAPI ループバック（相手＝PC の再生音）を**別 WAV** に落とす。
//! cpal は出力デバイスを入力として開くと透過的にループバックになる（NOTES.md 参照）。
//!
//! このプログラムの目的は「録れるか」ではなく **タイムラインが保たれるか**の計測。
//! 業務ノート（Realtek）ではループバックが**再生中しかパケットを返さない**ことが判っており、
//! サンプル数を時刻に換算する実装は壊れる（ADR-004）。ズレがどこで開いたかを追えるよう、
//! 1秒ごとの累積フレーム数と underrun の発生時刻を記録する。
//!
//! 使い方（Windows 側で。WSL では動かない）:
//!   run.cmd --list                        デバイス一覧を見る
//!   run.cmd --loopback=Realtek            **Ctrl+C まで録り続ける**（会議はこれ）
//!   run.cmd 30                            30 秒だけ録る（計測用）
//!
//! 出力は WAV 2本と、それぞれに対応する `*-meta.json`（採用したエンドポイント名・
//! 1秒ごとの累積フレーム・underrun 時刻）。後から「無音だったのはなぜか」を追うための材料。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// 音声コールバックから書き出しスレッドへ渡す1回ぶん。
struct Chunk {
    samples: Vec<f32>,
    /// 録音開始からの経過秒（到着時刻）。ADR-004 のためにサンプル数と別に持つ。
    at: f64,
}

/// 書き出しスレッドが返す集計。
struct Stats {
    frames_written: u64,
    channels_out: u16,
    peak: f32,
    rms: f64,
    downmixed: bool,
    /// ダウンミックス中に「4ch が同一」でなくなったフレーム数。0 でなければ前提が崩れている。
    downmix_violations: u64,
    /// パケットが来なかった区間 (検出時刻, 空いた秒数)。ADR-004 の埋めるべき穴そのもの。
    drops: Vec<(f64, f64)>,
}

fn dbfs(v: f64) -> f64 {
    if v <= 0.0 {
        -120.0
    } else {
        20.0 * v.log10()
    }
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "(名前不明)".into())
}

/// 名前に `needle` を含むデバイスを候補から探す。指定が無ければ `fallback` を使う。
fn pick(
    candidates: impl Iterator<Item = cpal::Device>,
    needle: Option<&str>,
    fallback: Option<cpal::Device>,
    kind: &str,
) -> Result<cpal::Device> {
    if let Some(needle) = needle {
        for device in candidates {
            if device_name(&device).contains(needle) {
                return Ok(device);
            }
        }
        eprintln!("  !! '{needle}' に一致する{kind}が無い。既定を使う。");
    }
    fallback.ok_or_else(|| anyhow!("{kind}が見つからない"))
}

/// 1フレームぶんの全チャンネルが同一値か。
fn frame_is_uniform(frame: &[f32]) -> bool {
    frame.windows(2).all(|w| w[0] == w[1])
}

/// 受け取ったサンプルをその場で WAV へ流す。
///
/// 溜めてから書くと、実測フォーマット（自分 48kHz×4ch / 相手 96kHz×2ch）では
/// 60分で約 5.5GB になり業務ノートで走らない。逐次書き出しが 60分テストの前提条件。
///
/// 業務ノートの内蔵マイクは 4ch だが**中身はモノラルの複製**なので、最初のチャンクで
/// それを確かめて 1ch に落とす。平均を取っても同じ値になるだけで、容量が4倍になる。
/// ダウンミックス判定で「無音でない」とみなすしきい値（約 -80 dBFS）。
const SILENCE: f32 = 1e-4;

fn spawn_writer(
    rx: Receiver<Chunk>,
    path: String,
    rate: u32,
    channels_in: u16,
    downmix_allowed: bool,
) -> JoinHandle<Result<Stats>> {
    std::thread::spawn(move || -> Result<Stats> {
        let ch_in = channels_in.max(1) as usize;
        let mut writer: Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>> = None;
        let mut downmix = false;
        let mut channels_out = channels_in;

        let mut frames_written: u64 = 0;
        let mut peak = 0.0f32;
        let mut sumsq = 0.0f64;
        let mut written_samples: u64 = 0;
        let mut violations: u64 = 0;
        // 到着時刻と「それまでに届いた音の長さ」の差。無音でパケットが止まると、
        // 止まっていた分だけこの差が一段増える。増えた時刻と量が、埋めるべき穴になる。
        let mut drops: Vec<(f64, f64)> = Vec::new();
        let mut last_lag = f64::NAN;

        // ダウンミックスの判定材料。最初の1チャンクだけで決めると、たまたま先頭に来た
        // モノラルの合図音や通知音を見て「全chが同一」と誤判定する（実際に起きた）。
        // 無音でないフレームが十分に集まるまで待ってから決める。
        //
        // なお**窓を広げても、ループバックでは足りない**。本編の再生が始まるのが数十秒後の
        // ことがあり、それまでの材料は合図音や通知音しかない。だから相手側は
        // `downmix_allowed = false` にして構造的に落とさない。窓の拡張はマイク側の保険。
        let mut pending: Vec<f32> = Vec::new();
        let mut pending_frames: u64 = 0;
        let mut probe_nonsilent: u64 = 0;
        let mut probe_nonuniform: u64 = 0;
        let decide_frames = rate as u64 * 2; // 2 秒ぶん見る
        let cap_frames = rate as u64 * 5; // これ以上は待たない
        let need_nonsilent = (rate / 5) as u64; // 0.2 秒ぶんの有音が判定の最低条件

        loop {
            let received = rx.recv().ok();
            let closed = received.is_none();

            let samples = match received {
                Some(c) if c.samples.is_empty() => continue,
                Some(c) => {
                            let lag = c.at - frames_written as f64 / rate as f64;
                    if last_lag.is_finite() {
                        if lag - last_lag > 0.2 {
                            drops.push((c.at, lag - last_lag));
                        }
                    } else if lag > 0.0 {
                        // 先頭の空白。録音開始から最初のデータまでも統合器には穴に見える。
                        // ここを入れないと「サンプル0 = first_data_sec」という第2の原点が要る。
                        // drops だけで復元できる形にしておく（誤差は約1バッファ）。
                        drops.push((c.at, lag));
                    }
                    last_lag = lag;
                    Some(c.samples)
                }
                None => None,
            };

            let to_write: Vec<f32> = if writer.is_none() {
                if let Some(s) = samples {
                    for frame in s.chunks_exact(ch_in) {
                        if frame.iter().any(|v| v.abs() > SILENCE) {
                            probe_nonsilent += 1;
                            if !frame_is_uniform(frame) {
                                probe_nonuniform += 1;
                            }
                        }
                    }
                    pending_frames += (s.len() / ch_in) as u64;
                    pending.extend_from_slice(&s);
                }
                let enough = probe_nonsilent >= need_nonsilent && pending_frames >= decide_frames;
                if !closed && !enough && pending_frames < cap_frames {
                    continue;
                }
                // 材料が足りないまま打ち切った時は落とさない（安全側）。
                downmix = downmix_allowed
                    && ch_in > 1
                    && probe_nonsilent >= need_nonsilent
                    && probe_nonuniform == 0;
                channels_out = if downmix { 1 } else { channels_in };
                let spec = hound::WavSpec {
                    channels: channels_out,
                    sample_rate: rate,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                };
                writer = Some(
                    hound::WavWriter::create(&path, spec)
                        .with_context(|| format!("{path} を作成できない"))?,
                );
                std::mem::take(&mut pending)
            } else {
                samples.unwrap_or_default()
            };

            if let Some(w) = writer.as_mut() {
                for frame in to_write.chunks_exact(ch_in) {
                    if downmix && !frame_is_uniform(frame) {
                        violations += 1;
                    }
                    let out: &[f32] = if downmix { &frame[..1] } else { frame };
                    for &s in out {
                        peak = peak.max(s.abs());
                        sumsq += (s as f64) * (s as f64);
                        written_samples += 1;
                        w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
                    }
                    frames_written += 1;
                }
            }

            if closed {
                break;
            }
        }

        // 一度もデータが来なかった系統でも、空の WAV は残す（「無かった」ことの記録になる）。
        let w = match writer {
            Some(w) => w,
            None => {
                let spec = hound::WavSpec {
                    channels: channels_in,
                    sample_rate: rate,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                };
                hound::WavWriter::create(&path, spec)
                    .with_context(|| format!("{path} を作成できない"))?
            }
        };
        w.finalize()?;

        let rms = if written_samples == 0 {
            0.0
        } else {
            (sumsq / written_samples as f64).sqrt()
        };
        Ok(Stats {
            frames_written,
            channels_out,
            peak,
            rms,
            downmixed: downmix,
            downmix_violations: violations,
            drops,
        })
    })
}

/// 録音中の1系統。ストリームは drop すると止まる。
struct Track {
    label: &'static str,
    device_name: String,
    path: String,
    rate: u32,
    channels_in: u16,
    /// ループバック側か。ズレの意味が系統で違うので、警告文言の出し分けに使う。
    loopback: bool,
    /// 要求したバッファ長（フレーム）。0 は既定（WASAPI の最小リング）。
    buffer_frames: u32,
    /// コールバックが受け取った累積フレーム数。1秒ごとに主スレッドが読む。
    frames_seen: Arc<AtomicU64>,
    /// underrun / overrun の発生時刻（開始からの秒）。回数だけでは原因が追えない。
    underruns: Arc<Mutex<Vec<f64>>>,
    first_data: Arc<Mutex<Option<f64>>>,
    /// (経過秒, 累積フレーム) の 1 秒ごとの記録。
    timeline: Vec<(f64, u64)>,
    stream: cpal::Stream,
    tx: Option<Sender<Chunk>>,
    writer: JoinHandle<Result<Stats>>,
}

impl Track {
    /// 録音を開始する。`loopback` が true なら出力デバイスを入力として開く。
    ///
    /// 設定の取り方が系統で違うのが罠。ループバックでは `default_input_config()` は
    /// 「このデバイスは入力に対応していない」で失敗する（cpal はデータフローで弾く）。
    /// 出力デバイスの設定は `default_output_config()` から取り、それで入力ストリームを建てる。
    fn start(
        device: cpal::Device,
        label: &'static str,
        loopback: bool,
        path: String,
        t0: Instant,
    ) -> Result<Self> {
        let config = if loopback {
            device.default_output_config()
        } else {
            device.default_input_config()
        }
        .with_context(|| format!("{label}: 既定の設定を取得できない"))?;

        let name = device_name(&device);
        let rate = config.sample_rate();
        let channels_in = config.channels();
        // 採用したエンドポイントは必ず残す。「無音だったのはなぜか」を後から追う起点になる。
        println!(
            "  [{label}] {name}\n          {rate} Hz / {channels_in} ch / {:?} → {path}",
            config.sample_format()
        );

        let sample_format = config.sample_format();
        let (tx, rx) = mpsc::channel::<Chunk>();
        // 相手（ループバック）は常にステレオ前提で残す。システム全体の再生音なので、
        // 通知音・動画・会議アプリが混ざり、モノラルである根拠が無い。
        let writer = spawn_writer(rx, path.clone(), rate, channels_in, !loopback);

        let frames_seen = Arc::new(AtomicU64::new(0));
        let underruns = Arc::new(Mutex::new(Vec::new()));
        let first_data = Arc::new(Mutex::new(None));

        // ストリームは2回建てうる（大きいバッファが通らなければ既定へ落とす）ので、
        // 状態のクローンは毎回この中で取る。
        let make = |cfg: cpal::StreamConfig| -> Result<cpal::Stream> {
            let err_times = Arc::clone(&underruns);
            let err_fn = move |e| {
                let at = t0.elapsed().as_secs_f64();
                let mut times = err_times.lock().unwrap();
                if times.is_empty() {
                    eprintln!("  !! [{label}] 録音エラー ({at:.2}秒): {e}");
                }
                times.push(at);
            };
            let sender = tx.clone();
            let seen = Arc::clone(&frames_seen);
            let first = Arc::clone(&first_data);
            let ch = channels_in.max(1) as u64;

            macro_rules! build {
                ($t:ty, $conv:expr) => {
                    device.build_input_stream(
                        cfg,
                        move |data: &[$t], _: &_| {
                            if data.is_empty() {
                                return;
                            }
                            let at = t0.elapsed().as_secs_f64();
                            let mut f = first.lock().unwrap();
                            if f.is_none() {
                                *f = Some(at);
                            }
                            drop(f);
                            seen.fetch_add(data.len() as u64 / ch, Ordering::Relaxed);
                            // コールバックごとに Vec を確保する。10ms 程度の粒度なので実用上
                            // 問題ないが、本実装ではリングバッファに置き換える余地がある。
                            let samples: Vec<f32> = data.iter().map($conv).collect();
                            let _ = sender.send(Chunk { samples, at });
                        },
                        err_fn,
                        None,
                    )?
                };
            }

            Ok(match sample_format {
                cpal::SampleFormat::F32 => build!(f32, |&s| s),
                cpal::SampleFormat::I16 => build!(i16, |&s| s as f32 / i16::MAX as f32),
                other => return Err(anyhow!("{label}: 未対応のサンプル形式 {other:?}")),
            })
        };

        // 既定のバッファは WASAPI の最小リングで、こちらが少しでも遅れると取りこぼす。
        // 70分で underrun 1045回・欠損7.25秒という実測が出た。コールバック周期は
        // GetDevicePeriod() 固定でバッファ長では変わらないので、**大きくしても遅延は増えず、
        // 取りこぼしまでの余裕だけが増える**。このPJは停止後に処理するので遅延は問題にならない。
        let mut cfg: cpal::StreamConfig = config.clone().into();
        cfg.buffer_size = cpal::BufferSize::Fixed(rate); // 1秒ぶん
        let (stream, buffer_frames) = match make(cfg) {
            Ok(s) => (s, rate),
            Err(e) => {
                eprintln!("  !! [{label}] 1秒バッファで開けない({e})。既定のバッファへ落とす。");
                let mut fallback: cpal::StreamConfig = config.clone().into();
                fallback.buffer_size = cpal::BufferSize::Default;
                (make(fallback)?, 0)
            }
        };
        stream.play()?;

        Ok(Self {
            label,
            device_name: name,
            path,
            rate,
            channels_in,
            loopback,
            buffer_frames,
            frames_seen,
            underruns,
            first_data,
            timeline: Vec::new(),
            stream,
            tx: Some(tx),
            writer,
        })
    }

    fn frames(&self) -> u64 {
        self.frames_seen.load(Ordering::Relaxed)
    }

    /// ストリームを止め、書き出しの完了を待って集計を報告する。
    fn finish(mut self, wall: f64) -> Result<()> {
        drop(self.stream); // 先に止める。以降コールバックは来ない。
        self.tx.take(); // 送信側を落とすと書き出しスレッドが抜ける。
        let stats = self
            .writer
            .join()
            .map_err(|_| anyhow!("{}: 書き出しスレッドが落ちた", self.label))??;

        let captured = stats.frames_written as f64 / self.rate as f64;
        let gap = wall - captured;
        let underruns = self.underruns.lock().unwrap().clone();
        let first = *self.first_data.lock().unwrap();

        println!("\n  [{}] {}", self.label, self.device_name);
        println!("    実経過      : {wall:.2} 秒");
        println!(
            "    録れた長さ  : {captured:.2} 秒 ({} フレーム)",
            stats.frames_written
        );
        // ズレの意味は系統で違う。相手は「鳴っていた時間しか来ない」ことの現れで、
        // 自分は「起動遅延 + underrun による欠損」。同じ文言を出すと読み違える。
        let note = if gap.abs() <= 0.5 {
            String::new()
        } else if self.loopback {
            "   ← 再生されていた時間ぶんしか来ていない。統合は時刻ベースで（ADR-004）".into()
        } else {
            format!(
                "   ← 起動遅延 {:.2}秒 + underrun {} 回ぶんの欠損か（1回あたり {:.1} ms）",
                first.unwrap_or(0.0),
                underruns.len(),
                if underruns.is_empty() {
                    0.0
                } else {
                    (gap - first.unwrap_or(0.0)) * 1000.0 / underruns.len() as f64
                }
            )
        };
        println!("    ズレ        : {gap:+.2} 秒{note}");
        match first {
            Some(at) => println!("    最初のデータ: 開始から {at:.2} 秒後"),
            None => println!("    最初のデータ: 一度も来なかった   ← このデバイスでは録れていない"),
        }
        println!(
            "    音量        : ピーク {:.1} dBFS / RMS {:.1} dBFS{}",
            dbfs(stats.peak as f64),
            dbfs(stats.rms),
            if stats.peak < 0.001 {
                "   ← ほぼ無音。デバイス選択と Windows の録音音量を疑う"
            } else {
                ""
            }
        );
        if stats.downmixed {
            if stats.downmix_violations > 0 {
                println!(
                    "    ダウンミックス: {}ch → 1ch   !! {} フレームが同一でなかった（全 {} 中）",
                    self.channels_in, stats.downmix_violations, stats.frames_written
                );
                println!("                    判定した区間だけで決めており、前提が崩れている");
            } else {
                println!(
                    "    ダウンミックス: {}ch → 1ch（判定した区間で全chが同一だったため）",
                    self.channels_in
                );
            }
        }
        if !stats.drops.is_empty() {
            let total: f64 = stats.drops.iter().map(|(_, d)| d).sum();
            println!(
                "    無通信区間  : {} 回 / 合計 {:.2} 秒   ← ここを時刻で埋める（ADR-004）",
                stats.drops.len(),
                total
            );
            for (at, d) in stats.drops.iter().take(8) {
                println!("                  {at:7.2}秒 で {d:.2} 秒ぶん");
            }
            if stats.drops.len() > 8 {
                println!("                  …他 {} 件（meta.json に全件）", stats.drops.len() - 8);
            }
        }
        println!(
            "    underrun    : {} 回{}",
            underruns.len(),
            match (underruns.first(), underruns.last()) {
                (Some(a), Some(b)) if underruns.len() > 1 => format!("（{a:.1}秒〜{b:.1}秒）"),
                (Some(a), _) => format!("（{a:.1}秒）"),
                _ => String::new(),
            }
        );
        println!("    出力        : {}", self.path);

        let meta = format!(
            "{{\n  \"label\": \"{}\",\n  \"device\": \"{}\",\n  \"wav\": \"{}\",\n  \
             \"sample_rate\": {},\n  \"channels_device\": {},\n  \"channels_written\": {},\n  \
             \"buffer_frames_requested\": {},\n  \
             \"downmixed\": {},\n  \"downmix_violations\": {},\n  \"frames_written\": {},\n  \
             \"captured_sec\": {:.3},\n  \"wall_elapsed_sec\": {:.3},\n  \"gap_sec\": {:.3},\n  \
             \"first_data_sec\": {},\n  \"peak_dbfs\": {:.1},\n  \"rms_dbfs\": {:.1},\n  \
             \"underrun_sec\": [{}],\n  \"drops_sec_len\": [{}],\n  \
             \"timeline_sec_frames\": [{}]\n}}\n",
            json_escape(self.label),
            json_escape(&self.device_name),
            json_escape(&self.path),
            self.rate,
            self.channels_in,
            stats.channels_out,
            self.buffer_frames,
            stats.downmixed,
            stats.downmix_violations,
            stats.frames_written,
            captured,
            wall,
            gap,
            first.map(|v| format!("{v:.3}")).unwrap_or("null".into()),
            dbfs(stats.peak as f64),
            dbfs(stats.rms),
            underruns
                .iter()
                .map(|v| format!("{v:.3}"))
                .collect::<Vec<_>>()
                .join(", "),
            stats
                .drops
                .iter()
                .map(|(at, d)| format!("[{at:.3}, {d:.3}]"))
                .collect::<Vec<_>>()
                .join(", "),
            self.timeline
                .iter()
                .map(|(t, f)| format!("[{t:.3}, {f}]"))
                .collect::<Vec<_>>()
                .join(", "),
        );
        let meta_path = self
            .path
            .strip_suffix(".wav")
            .map(|s| format!("{s}-meta.json"))
            .unwrap_or_else(|| format!("{}-meta.json", self.path));
        std::fs::write(&meta_path, meta).with_context(|| format!("{meta_path} を書けない"))?;
        println!("    メタ        : {meta_path}");
        Ok(())
    }
}

fn json_escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            '"' => vec!['\\', '"'],
            '\\' => vec!['\\', '\\'],
            '\n' => vec!['\\', 'n'],
            c => vec![c],
        })
        .collect()
}

/// 途中終了で書きかけになった WAV のヘッダを直す。
///
/// hound は作成時にサイズ欄を 0 で書き、`finalize()` で埋める。強制終了されると
/// 埋められないまま残り、**データはあるのに「not a WAVE file」で読めない**（実測）。
/// 形式（チャンネル・レート・ビット数）はヘッダに残っているので、
/// 実ファイルサイズから2つのサイズ欄を書き直すだけで戻る。
fn repair_wav(path: &str) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write as _};
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("{path} を開けない"))?;
    let size = f.metadata()?.len();
    if size < 44 {
        bail!("{path} は 44 バイト未満。中身が無い。");
    }
    let mut head = [0u8; 44];
    f.read_exact(&mut head)?;
    if &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        bail!("{path} は WAV ではない");
    }
    let data_len = (size - 44) as u32;
    let riff_len = (size - 8) as u32;
    let ch = u16::from_le_bytes([head[22], head[23]]);
    let rate = u32::from_le_bytes([head[24], head[25], head[26], head[27]]);
    let bits = u16::from_le_bytes([head[34], head[35]]);

    f.seek(SeekFrom::Start(4))?;
    f.write_all(&riff_len.to_le_bytes())?;
    f.seek(SeekFrom::Start(40))?;
    f.write_all(&data_len.to_le_bytes())?;

    let secs = data_len as f64 / (rate as f64 * ch as f64 * (bits as f64 / 8.0));
    println!("{path} を直した: {rate} Hz / {ch}ch / {bits}bit / {secs:.2} 秒");
    println!("  !! 途中終了した録音なので `*-meta.json` が無い（または不完全）。");
    println!("     時刻の補正ができないため、統合すると WAV 内の時刻のまま並ぶ。");
    Ok(())
}

fn list_devices() -> Result<()> {
    let host = cpal::default_host();
    println!("host: {:?}\n", host.id());

    let default_in = host.default_input_device().map(|d| device_name(&d));
    println!("入力デバイス（マイク＝自分の声）:");
    for device in host.input_devices()? {
        let name = device_name(&device);
        let mark = if Some(&name) == default_in.as_ref() { "  ← 既定" } else { "" };
        match device.default_input_config() {
            Ok(c) => println!("  {name}{mark}\n      {} Hz / {} ch / {:?}", c.sample_rate(), c.channels(), c.sample_format()),
            Err(e) => println!("  {name}{mark}\n      設定を取得できない: {e}"),
        }
    }

    let default_out = host.default_output_device().map(|d| device_name(&d));
    println!("\n出力デバイス（ループバック＝相手の声。会議音が実際に出ている先を選ぶ）:");
    for device in host.output_devices()? {
        let name = device_name(&device);
        let mark = if Some(&name) == default_out.as_ref() { "  ← 既定" } else { "" };
        match device.default_output_config() {
            Ok(c) => println!("  {name}{mark}\n      {} Hz / {} ch / {:?}", c.sample_rate(), c.channels(), c.sample_format()),
            Err(e) => println!("  {name}{mark}\n      設定を取得できない: {e}"),
        }
    }
    println!("\n※ ヘッドホンジャックのエンドポイントは抜き差しで一覧から出入りし、既定が");
    println!("   耳に届かないデバイスへ移る。`--loopback=<名前の一部>` で固定すること（ADR-005）。");
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--list") {
        return list_devices();
    }
    if let Some(i) = args.iter().position(|a| a == "--repair") {
        let target = args
            .get(i + 1)
            .ok_or_else(|| anyhow!("--repair のあとに WAV のパスを渡す"))?;
        return repair_wav(target);
    }

    // 会議は長さが読めない。秒数を省いたら Ctrl+C まで録り続ける。
    // 数字を渡すのは計測のときだけ。
    let secs: Option<u64> = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .and_then(|a| a.parse().ok());
    let opt = |key: &str| {
        args.iter()
            .find_map(|a| a.strip_prefix(key).map(|v| v.to_string()))
    };
    let mic_needle = opt("--mic=");
    let loop_needle = opt("--loopback=");

    let host = cpal::default_host();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    match secs {
        Some(n) => println!("2系統同時録音（{n} 秒）\n"),
        None => println!("2系統同時録音（Ctrl+C で停止）\n"),
    }

    // Ctrl+C を捕まえて、書き出しを正常に閉じてから終わる。捕まえないと
    // hound がヘッダを書けず、データが残っていても読めない WAV になる。
    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let stop = Arc::clone(&running);
    ctrlc::set_handler(move || {
        stop.store(false, Ordering::SeqCst);
    })
    .context("Ctrl+C のハンドラを設定できない")?;

    // Ctrl+C は環境によっては届かない（WSL から起動した exe など）。
    // 信号に依存しない停止手段として、Enter でも止まるようにしておく。
    // **止め方が1つしか無いと、それが効かなかった時に録音を丸ごと失う。**
    let stop_key = Arc::clone(&running);
    std::thread::spawn(move || {
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_ok() {
            stop_key.store(false, Ordering::SeqCst);
        }
    });

    // 2系統の時刻を突き合わせるので、時計は1つにする。
    let t0 = Instant::now();

    // マイクが無い機械（開発デスクトップ等）でもループバック側だけは測れるようにする。
    let mut mic_track = match pick(
        host.input_devices()?,
        mic_needle.as_deref(),
        host.default_input_device(),
        "入力デバイス",
    ) {
        Ok(mic) => Some(Track::start(
            mic,
            "自分",
            false,
            format!("dual-{stamp}-self.wav"),
            t0,
        )?),
        Err(e) => {
            eprintln!("  !! マイクを開けない({e})。ループバックだけで続ける。");
            None
        }
    };
    let spk = pick(
        host.output_devices()?,
        loop_needle.as_deref(),
        host.default_output_device(),
        "出力デバイス",
    )?;
    let mut loop_track = Track::start(spk, "相手", true, format!("dual-{stamp}-other.wav"), t0)?;

    println!("\n録音中… 1秒ごとに累積フレーム数を出す（実時刻とサンプル数の乖離を追うため）。");
    if secs.is_none() {
        println!("停止するときは Ctrl+C か Enter。**強制終了させると WAV が読めなくなる**");
        println!("（読めなくなっても `--repair <ファイル>` で戻せる。時刻の補正だけ失う）");
    }
    // 1秒ごとに両系統の累積を読む。ズレが「じわじわ開いた（ドリフト）」のか
    // 「一度に飛んだ（欠損）」のかは、最終サマリだけでは区別できない。
    let mut elapsed_ticks = 0u64;
    while running.load(Ordering::SeqCst) && secs.map_or(true, |n| elapsed_ticks < n) {
        std::thread::sleep(Duration::from_secs(1));
        elapsed_ticks += 1;
        let at = t0.elapsed().as_secs_f64();
        let lf = loop_track.frames();
        loop_track.timeline.push((at, lf));
        let mf = match mic_track.as_mut() {
            Some(t) => {
                let f = t.frames();
                t.timeline.push((at, f));
                format!("{:>10}", f)
            }
            None => "         -".into(),
        };
        // 秒数 / 自分の累積フレーム / 相手の累積フレーム
        println!("  {at:7.1}秒  自分 {mf}  相手 {lf:>10}");
    }
    let wall = t0.elapsed().as_secs_f64();
    if !running.load(Ordering::SeqCst) {
        println!("\n停止の指示を受けた。書き出しを閉じる。");
    } else {
        println!("\n停止。");
    }

    if let Some(mic_track) = mic_track {
        mic_track.finish(wall)?;
    }
    loop_track.finish(wall)?;

    println!("\n見るべき点:");
    println!("  1. 相手のズレ — 再生していた時間と録れた長さが一致するか（ADR-004 の根拠）");
    println!("  2. 自分のズレ − 起動遅延 が underrun 件数 × 数ms に収まるか（ADR-006）");
    println!("  3. underrun の回数 — バッファを1秒にした効果が出ているか");
    Ok(())
}
