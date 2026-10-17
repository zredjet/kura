//! 照合用の C エンコーダ（ADR-149）を呼んで、パケット・ブロックごとの中間値のダンプ・設定の JSON を読む（テストだけ）。
//!
//! - 照合用のエンコーダの作り方は `crates/vorbis-tables/oracle/`（README.md）。C のソースの写しはリポジトリに入れず、
//!   crates.io の `aotuv_lancer_vorbis_sys` 0.1.6 の手を入れていない源に、そこの差分と新しいファイルを足して組む：
//!
//!   ```sh
//!   python3 -I crates/vorbis-tables/oracle/build.py        # 出力は target/vorbis-oracle/（bin/・test/in/）
//!   KURA_VORBIS_ORACLE=target/vorbis-oracle cargo test --release -p kura-engine --lib codec::vorbis
//!   ```
//!
//! - 環境変数 `KURA_VORBIS_ORACLE` に照合用のエンコーダの出力先（`bin/oracle_enc`・`bin/oracle_setup`・`test/in/` が
//!   ある所。相対の道はリポジトリの根から）を入れたときだけ動く（[`root`]）。ないとき（空のときも）は `None` を返し、照合の
//!   テストは何もせずに通る（CI では回らない。移植の作業の手元で回す）。入れたのに `bin/oracle_enc`（NaN の入力のテストでは
//!   `test/in/` の入力）がなければ止まる。
//! - 形式は照合用のエンコーダの README（§4）のとおり：入力は planar の f32、パケットのファイルは
//!   `u32 長さ・i64 granulepos・u8 旗・本体`、ダンプは 12 バイトの見出し（tag・ch・type・len）＋値の列。
//! - 照合用のエンコーダは aarch64 で `-ffp-contract=off`・`-fwrapv`（符号付き整数のあふれは 2 の補数の折り返し）・超越関数は
//!   Rust の libm =0.2.16、入力は一度に渡し、aoTuV の前増幅だけを切ったもの（既定の `KURA_ABL=PREAMP`。`vorbis/mod.rs` の
//!   冒頭）。`-fwrapv` のない前の組では極端な振幅の照合（`stream_tests::oracle_stream_extreme_amplitudes`）が落ちるので、
//!   `build.py` で組み直す（組んだときのフラグは出力先の `build-info.txt` の `cflags:`）。
//! - `KURA_FFMPEG`（encoder.rs のテスト）：あれば `oracle_mux_ffmpeg_lengths` が ffmpeg でも長さと値を確かめる。

// 照合のテストの部品（使わない口もある）
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods
)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use rand_chacha::rand_core::Rng;

/// 照合用のエンコーダの出力先（`crates/vorbis-tables/oracle/build.py` の `--out`。既定は `target/vorbis-oracle`）。
///
/// 環境変数 `KURA_VORBIS_ORACLE` がない・空なら None（照合のテストは何もせずに通る）。相対の道はリポジトリの根からとみなす
/// （`cargo test` はクレートのディレクトリで走るので、そのままだと `crates/kura-engine/` からになってしまう）。
/// 入れたのに `bin/oracle_enc` がないときは、照合を回したつもりで何もせずに通らないよう止まる。
pub(crate) fn root() -> Option<PathBuf> {
    let dir = std::env::var_os("KURA_VORBIS_ORACLE").filter(|v| !v.is_empty())?;
    let dir = PathBuf::from(dir);
    let dir = if dir.is_relative() {
        // CARGO_MANIFEST_DIR は crates/kura-engine。その 2 つ上がリポジトリの根
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        manifest.ancestors().nth(2).unwrap_or(manifest).join(dir)
    } else {
        dir
    };
    assert!(
        dir.join("bin/oracle_enc").exists(),
        "KURA_VORBIS_ORACLE={} に bin/oracle_enc がありません（crates/vorbis-tables/oracle/build.py で組む）",
        dir.display()
    );
    Some(dir)
}

/// 一時ファイルの名前（プロセスと通し番号で重ならない）。
fn temp(name: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "kura-vorbis-oracle-{}-{n}-{name}",
        std::process::id()
    ))
}

/// パケット（照合用のエンコーダのパケットのファイルの 1 つ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Packet {
    pub body: Vec<u8>,
    pub granule: i64,
    /// ビット 0 = b_o_s、ビット 1 = e_o_s。
    pub flags: u8,
}

/// ダンプの記録 1 つ。
#[derive(Debug, Clone)]
pub(crate) struct Record {
    pub tag: [u8; 4],
    /// チャンネル（ブロック全体の記録は 0xFFFF。RCLS は submap × 256 ＋ 束の中の番号）。
    pub ch: u16,
    /// 1 = i32、2 = f32 のビット列、3 = i64。
    pub kind: u16,
    pub data: Vec<u8>,
}

impl Record {
    /// タグ（4 文字。空白で埋めたもの）が `t` か。
    pub fn is(&self, t: &str) -> bool {
        let mut want = [b' '; 4];
        for (d, s) in want.iter_mut().zip(t.bytes()) {
            *d = s;
        }
        self.tag == want
    }

    pub fn i32s(&self) -> Vec<i32> {
        self.data
            .chunks_exact(4)
            .map(|b| i32::from_le_bytes(b.try_into().unwrap()))
            .collect()
    }

    /// f32 をビット列のまま。
    pub fn f32_bits(&self) -> Vec<u32> {
        self.data
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect()
    }

    pub fn f32s(&self) -> Vec<f32> {
        self.f32_bits().into_iter().map(f32::from_bits).collect()
    }

    pub fn i64s(&self) -> Vec<i64> {
        self.data
            .chunks_exact(8)
            .map(|b| i64::from_le_bytes(b.try_into().unwrap()))
            .collect()
    }
}

/// ダンプ：先頭の `KDMP`（i64 × 8）と、ブロックごとの記録（`BLCK`〜`BEND`）。
#[derive(Debug, Clone, Default)]
pub(crate) struct Dump {
    pub header: Vec<i64>,
    pub blocks: Vec<Vec<Record>>,
}

impl Dump {
    /// ブロック `b` の、タグ `t`・チャンネル `ch` の記録。
    pub fn get(&self, b: usize, t: &str, ch: u16) -> Option<&Record> {
        self.blocks.get(b)?.iter().find(|r| r.is(t) && r.ch == ch)
    }

    /// ブロック `b` の `BLCK`（通し番号・W・lW・nW・blocktype・block_mode・granulepos・packetno・n・eofflag）。
    pub fn block_info(&self, b: usize) -> Vec<i64> {
        self.get(b, "BLCK", 0xFFFF)
            .map(Record::i64s)
            .unwrap_or_default()
    }
}

fn read_packets(bytes: &[u8]) -> Vec<Packet> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let granule = i64::from_le_bytes(bytes[at + 4..at + 12].try_into().unwrap());
        let flags = bytes[at + 12];
        out.push(Packet {
            body: bytes[at + 13..at + 13 + len].to_vec(),
            granule,
            flags,
        });
        at += 13 + len;
    }
    out
}

fn read_dump(bytes: &[u8]) -> Dump {
    let mut dump = Dump::default();
    let mut at = 0usize;
    let mut cur: Vec<Record> = Vec::new();
    while at + 12 <= bytes.len() {
        let tag: [u8; 4] = bytes[at..at + 4].try_into().unwrap();
        let ch = u16::from_le_bytes(bytes[at + 4..at + 6].try_into().unwrap());
        let kind = u16::from_le_bytes(bytes[at + 6..at + 8].try_into().unwrap());
        let len = u32::from_le_bytes(bytes[at + 8..at + 12].try_into().unwrap()) as usize;
        let rec = Record {
            tag,
            ch,
            kind,
            data: bytes[at + 12..at + 12 + len].to_vec(),
        };
        at += 12 + len;
        if rec.is("KDMP") {
            dump.header = rec.i64s();
        } else if rec.is("BEND") {
            cur.push(rec);
            dump.blocks.push(std::mem::take(&mut cur));
        } else {
            cur.push(rec);
        }
    }
    dump
}

/// 照合用のエンコーダで書く。`planes` はチャンネルごとの標本（Vorbis のチャンネルの並び）、`q` は −1〜10、
/// `dump_level` が Some ならブロックごとの中間値も書き出す（1 か 2）。照合用のエンコーダがなければ None。
pub(crate) fn encode(
    planes: &[Vec<f32>],
    rate: u32,
    q: i32,
    dump_level: Option<u8>,
) -> Option<(Vec<Packet>, Option<Dump>)> {
    let root = root()?;
    let input = temp("in.f32");
    let output = temp("out.pkt");
    let dumpfile = temp("dump.kdmp");
    let mut raw = Vec::new();
    for p in planes {
        for s in p {
            raw.extend_from_slice(&s.to_le_bytes());
        }
    }
    std::fs::write(&input, raw).unwrap();
    let mut cmd = Command::new(root.join("bin/oracle_enc"));
    cmd.arg(&input)
        .arg(&output)
        .arg(rate.to_string())
        .arg(planes.len().to_string())
        .arg(q.to_string())
        .env_remove("KURA_ABL");
    if let Some(level) = dump_level {
        cmd.env("KURA_DUMP", &dumpfile)
            .env("KURA_DUMP_LEVEL", level.to_string());
    } else {
        cmd.env_remove("KURA_DUMP");
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "oracle_enc が失敗しました：{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let packets = read_packets(&std::fs::read(&output).unwrap());
    let dump = dump_level.map(|_| read_dump(&std::fs::read(&dumpfile).unwrap()));
    for f in [&input, &output, &dumpfile] {
        let _ = std::fs::remove_file(f);
    }
    Some((packets, dump))
}

/// 照合用の `oracle_setup` の JSON（浮動小数はビット列の文字列 `"0x…"`）。`look` で導出の表も出す。
pub(crate) fn setup(rate: u32, channels: usize, q: i32, look: bool) -> Option<serde_json::Value> {
    let root = root()?;
    let mut cmd = Command::new(root.join("bin/oracle_setup"));
    cmd.arg(rate.to_string())
        .arg(channels.to_string())
        .arg(q.to_string())
        .arg("--bits")
        .env_remove("KURA_ABL");
    if look {
        cmd.arg("--look");
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "oracle_setup が失敗しました：{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(serde_json::from_slice(&out.stdout).unwrap())
}

/// JSON の f32（`"0x%08x"` のビット列）。
pub(crate) fn json_f32(v: &serde_json::Value) -> f32 {
    let s = v.as_str().unwrap();
    f32::from_bits(u32::from_str_radix(s.trim_start_matches("0x"), 16).unwrap())
}

/// JSON の f64（`"0x%016llx"` のビット列）。
pub(crate) fn json_f64(v: &serde_json::Value) -> f64 {
    let s = v.as_str().unwrap();
    f64::from_bits(u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap())
}

/// 照合に使う入力の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signal {
    /// 和音と雑音と打楽器のような立ち上がりを混ぜたもの（窓の切り替え・トーンとノイズのマスクの両方に届く）。
    Music,
    /// 一様な雑音。
    Noise,
    /// 1 kHz の正弦波。
    Sine,
    /// 無音。
    Silence,
    /// 間をあけた単発の衝撃（短い窓）。
    Impulses,
    /// 間をあけた衝撃（16 標本の雑音）と、そのあと約 4.5 ms だけ続いて急に止まる 700 Hz の音（aoTuV の M2（衝撃の
    /// あとの後エコー。q0 で届く）・M3 に届く）。
    Bursts,
}

/// 照合に使う入力（決定的。チャンネルごとに少しずつ違う）。
pub(crate) fn signal(
    kind: Signal,
    rate: u32,
    channels: usize,
    frames: usize,
    seed: u64,
) -> Vec<Vec<f32>> {
    let two_pi = 2.0 * std::f64::consts::PI;
    (0..channels)
        .map(|ch| {
            let mut r = crate::determinism::rng(seed.wrapping_mul(31).wrapping_add(ch as u64));
            let mut unit = || f64::from(r.next_u32()) / 4_294_967_296.0 * 2.0 - 1.0;
            (0..frames)
                .map(|i| {
                    let t = i as f64 / f64::from(rate);
                    let v = match kind {
                        Signal::Silence => 0.0,
                        Signal::Sine => 0.5 * crate::math::sin(two_pi * 1_000.0 * t),
                        Signal::Noise => 0.4 * unit(),
                        Signal::Impulses => {
                            let period = (rate / 5).max(1) as usize;
                            if i % period < 8 { 0.8 * unit() } else { 0.0 }
                        }
                        Signal::Bursts => {
                            let period = (rate / 3).max(1) as usize;
                            let tail = (rate as usize * 200 / 44_100).max(1);
                            let k = i % period;
                            if k < 16 {
                                0.95 * unit()
                            } else if k < 16 + tail {
                                0.3 * crate::math::sin(two_pi * 700.0 * t)
                            } else {
                                0.0
                            }
                        }
                        Signal::Music => {
                            let f0 = 220.0 * (1.0 + 0.01 * ch as f64);
                            let tone = 0.2 * crate::math::sin(two_pi * f0 * t)
                                + 0.12 * crate::math::sin(two_pi * f0 * 1.5 * t)
                                + 0.08 * crate::math::sin(two_pi * f0 * 2.0 * t + 0.3);
                            let beat = (rate / 2).max(1) as usize;
                            let hit = if i % beat < (rate as usize / 100).max(1) {
                                0.5 * unit()
                            } else {
                                0.02 * unit()
                            };
                            tone + hit
                        }
                    };
                    v as f32
                })
                .collect()
        })
        .collect()
}

/// 2 つのパケットの列を比べ、最初に違う所を説明する（同じなら None）。`skip_comment` でコメントのパケット（1 番）を飛ばす。
pub(crate) fn first_difference(a: &[Packet], b: &[Packet], skip_comment: bool) -> Option<String> {
    if a.len() != b.len() {
        let n = a.len().min(b.len());
        for i in 0..n {
            if !(skip_comment && i == 1) && a[i] != b[i] {
                return Some(describe(i, &a[i], &b[i]));
            }
        }
        return Some(format!("パケットの数が違う：{} と {}", a.len(), b.len()));
    }
    (0..a.len())
        .find(|&i| !(skip_comment && i == 1) && a[i] != b[i])
        .map(|i| describe(i, &a[i], &b[i]))
}

fn describe(i: usize, a: &Packet, b: &Packet) -> String {
    let bit = a
        .body
        .iter()
        .zip(&b.body)
        .enumerate()
        .find(|(_, (x, y))| x != y)
        .map(|(k, (x, y))| k * 8 + (x ^ y).trailing_zeros() as usize);
    format!(
        "パケット {i}（音声 #{}）：{} と {} バイト、granule {} と {}、最初に違うビット {:?}",
        i.saturating_sub(3),
        a.body.len(),
        b.body.len(),
        a.granule,
        b.granule,
        bit
    )
}
