//! 2系統同時録音の最小プロトタイプ（Windows 専用）。
//!
//! マイク（自分）と WASAPI ループバック（相手＝PC の再生音）を**別 WAV** に落とす。
//! cpal は出力デバイスを入力として開くと透過的にループバックになる（NOTES.md 参照）。
//!
//! ただしこのプログラムの本命は「録れるか」ではない。録れることはほぼ判っている。
//! 確かめたいのは **タイムラインが保たれるか**（NOTES.md のリスク2）:
//! ループバックは再生中のアプリが無いとパケットを返さない可能性があり、
//! そうなるとサンプル数が経過時間より短くなり、統合時に自分トラックとずれる。
//! そこで経過時間と実サンプル数を突き合わせ、その差を数字で出す。
//!
//! 使い方（Windows 側で。WSL では動かない）:
//!   cargo run --release -- --list        デバイス一覧を見る
//!   cargo run --release -- 30            30 秒録って 2 本の WAV を書く
//!   cargo run --release -- 30 --mic=ThinkPad --loopback=Headset
//!
//! 検証手順: 録音を始めたら、最初の 10 秒は**何も再生せず自分だけ喋る**。
//! 残りで相手側（動画でも可）を鳴らす。無音区間の扱いはこの前半に出る。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// 録音中の1系統。ストリームは drop すると止まるので手放さずに持つ。
struct Track {
    label: &'static str,
    device_name: String,
    samples: Arc<Mutex<Vec<f32>>>,
    rate: u32,
    channels: u16,
    callbacks: Arc<AtomicUsize>,
    errors: Arc<AtomicUsize>,
    first_data: Arc<Mutex<Option<Instant>>>,
    started: Instant,
    stream: cpal::Stream,
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

impl Track {
    /// 録音を開始する。`loopback` が true なら出力デバイスを入力として開く。
    ///
    /// 設定の取り方が系統で違うのが罠。ループバックでは `default_input_config()` は
    /// 「このデバイスは入力に対応していない」で失敗する（cpal は data_flow で弾く）。
    /// 出力デバイスの設定は `default_output_config()` から取り、それで入力ストリームを建てる。
    fn start(device: cpal::Device, label: &'static str, loopback: bool) -> Result<Self> {
        let config = if loopback {
            device.default_output_config()
        } else {
            device.default_input_config()
        }
        .with_context(|| format!("{label}: 既定の設定を取得できない"))?;

        let name = device_name(&device);
        let rate = config.sample_rate();
        let channels = config.channels();
        println!(
            "  [{label}] {name}\n          {rate} Hz / {channels} ch / {:?}",
            config.sample_format()
        );

        // コールバック内で再確保が起きないよう 5 分ぶん先に確保する。
        // realloc は音声スレッドを止めるので、オーバーランの原因になる。
        let samples = Arc::new(Mutex::new(Vec::<f32>::with_capacity(
            rate as usize * channels.max(1) as usize * 300,
        )));
        let callbacks = Arc::new(AtomicUsize::new(0));
        let errors = Arc::new(AtomicUsize::new(0));
        let first_data = Arc::new(Mutex::new(None));

        let err_count = Arc::clone(&errors);
        let err_fn = move |e| {
            // エラーは連続して出るので最初の1件だけ表示し、以降は数える。
            if err_count.fetch_add(1, Ordering::Relaxed) == 0 {
                eprintln!("  !! [{label}] 録音エラー: {e}");
            }
        };

        let sink = Arc::clone(&samples);
        let cb_count = Arc::clone(&callbacks);
        let first = Arc::clone(&first_data);
        let started = Instant::now();

        macro_rules! build {
            ($t:ty, $conv:expr) => {
                device.build_input_stream(
                    config.clone().into(),
                    move |data: &[$t], _: &_| {
                        cb_count.fetch_add(1, Ordering::Relaxed);
                        if !data.is_empty() {
                            let mut f = first.lock().unwrap();
                            if f.is_none() {
                                *f = Some(Instant::now());
                            }
                        }
                        let mut buf = sink.lock().unwrap();
                        buf.extend(data.iter().map($conv));
                    },
                    err_fn,
                    None,
                )?
            };
        }

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => build!(f32, |&s| s),
            cpal::SampleFormat::I16 => build!(i16, |&s| s as f32 / i16::MAX as f32),
            other => return Err(anyhow!("{label}: 未対応のサンプル形式 {other:?}")),
        };
        stream.play()?;

        Ok(Self {
            label,
            device_name: name,
            samples,
            rate,
            channels,
            callbacks,
            errors,
            first_data,
            started,
            stream,
        })
    }

    /// WAV に書き出し、タイムラインのズレを報告する。
    fn finish(self, path: &str, elapsed: Duration) -> Result<()> {
        drop(self.stream); // 先に止める。以降バッファは増えない。
        let buf = self.samples.lock().unwrap();

        let frames = buf.len() / self.channels.max(1) as usize;
        let captured = frames as f64 / self.rate as f64;
        let wall = elapsed.as_secs_f64();
        let gap = wall - captured;

        // 「録れているつもりで無音」が最悪の失敗なので、必ず中身の大きさを見る。
        let peak = buf.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let rms = if buf.is_empty() {
            0.0
        } else {
            (buf.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / buf.len() as f64).sqrt()
        };
        let dbfs = |v: f64| if v <= 0.0 { -120.0 } else { 20.0 * v.log10() };

        println!("\n  [{}] {}", self.label, self.device_name);
        println!("    実経過      : {wall:.2} 秒");
        println!("    録れた長さ  : {captured:.2} 秒 ({frames} フレーム)");
        println!(
            "    ズレ        : {gap:+.2} 秒{}",
            if gap.abs() > 0.5 {
                "   ← 要注意。統合時に自分トラックとずれる"
            } else {
                ""
            }
        );
        if let Some(first) = *self.first_data.lock().unwrap() {
            println!(
                "    最初のデータ: 開始から {:.2} 秒後",
                first.duration_since(self.started).as_secs_f64()
            );
        } else {
            println!("    最初のデータ: 一度も来なかった   ← このデバイスでは録れていない");
        }
        println!(
            "    音量        : ピーク {:.1} dBFS / RMS {:.1} dBFS{}",
            dbfs(peak as f64),
            dbfs(rms),
            if peak < 0.001 { "   ← ほぼ無音。デバイス選択を疑う" } else { "" }
        );
        println!(
            "    コールバック: {} 回 / エラー {} 回",
            self.callbacks.load(Ordering::Relaxed),
            self.errors.load(Ordering::Relaxed)
        );

        // 素の 16bit PCM で、レート・チャンネル数はデバイスのまま書く。
        // ここで変換すると、後で疑う対象が増える。
        let spec = hound::WavSpec {
            channels: self.channels,
            sample_rate: self.rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec)
            .with_context(|| format!("{path} を作成できない"))?;
        for &s in buf.iter() {
            writer.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
        }
        writer.finalize()?;
        println!("    出力        : {path}");
        Ok(())
    }
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
    println!("\n※ Teams は通常の再生デバイスとは別に「通信デバイス」を持てる。");
    println!("   既定を機械的に選ぶと無音の WAV ができるので、会議中に実際に音が出ている先を確かめる。");
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--list") {
        return list_devices();
    }

    let secs: u64 = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .and_then(|a| a.parse().ok())
        .unwrap_or(30);
    let opt = |key: &str| {
        args.iter()
            .find_map(|a| a.strip_prefix(key).map(|v| v.to_string()))
    };
    let mic_needle = opt("--mic=");
    let loop_needle = opt("--loopback=");

    let host = cpal::default_host();
    println!("2系統同時録音（{secs} 秒）\n");

    // マイクが無い機械（開発デスクトップ等）でもループバック側だけは測れるようにする。
    // 無音時にパケットが来るかの確認はマイクと無関係なので、ここで落とすと検証ができない。
    let mic_track = match pick(
        host.input_devices()?,
        mic_needle.as_deref(),
        host.default_input_device(),
        "入力デバイス",
    ) {
        Ok(mic) => Some(Track::start(mic, "自分", false)?),
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
    let loop_track = Track::start(spk, "相手", true)?;

    // 2本のストリームは別々のクロックで動く。開始時刻の差はここでは詰められないので、
    // まとめて計時し、後段の統合はサンプル数ではなく時刻で行う前提にする。
    let t0 = Instant::now();
    println!("\n録音中… 前半は何も再生せず自分だけ喋り、後半で相手側の音を鳴らす。");
    std::thread::sleep(Duration::from_secs(secs));
    let elapsed = t0.elapsed();
    println!("\n停止。");

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    if let Some(mic_track) = mic_track {
        mic_track.finish(&format!("dual-{stamp}-self.wav"), elapsed)?;
    }
    loop_track.finish(&format!("dual-{stamp}-other.wav"), elapsed)?;

    println!("\n見るべき点:");
    println!("  1. 相手トラックの「ズレ」が 0 に近いか。大きければ無音時にパケットが来ていない");
    println!("     → 統合は時刻ベースで無音を埋める設計が必須になる");
    println!("  2. 自分トラックに相手の声が入っていないか（ヘッドセットなら入らないはず）");
    println!("  3. 2本のレート・チャンネル数の違い（統合時に揃える対象）");
    Ok(())
}
