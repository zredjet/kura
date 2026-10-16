//! aseprite の合成を、別の実装（asefile。Aseprite の blend_funcs.cpp の Rust への移植）と照らす（docs/editing.md §9）。
//!
//! 自前の書き手（testfiles::FileGen::Aseprite）でランダムな絵を作り、自前のデコーダと asefile の両方で読んで、
//! 各フレームの RGBA が一致することを確かめる。asefile が扱わないもの（グループの別の合成・z-index・背景の層・タイルの反転）は
//! 入れない（それらは src/codec/aseprite.rs のテストで確かめる）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Cursor;

use kura_engine::codec::CodecSet;
use kura_engine::doc::DecodeOptions;
use kura_engine::imaging::convert::to_rgba8_rows;
use kura_engine::testfiles::{AseCel, AseFrame, AseLayer, FileGen};
use kura_engine::{Limits, NoMonitor, Value};
use rand_chacha::rand_core::Rng;

/// 完全に透明な画素の RGB は見えず、その後の合成にも効かない（式が下か上のアルファ 0 の色を使わない）ので、比べる前に 0 にそろえる。
fn clear_invisible(mut frames: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    for f in &mut frames {
        for p in f.chunks_exact_mut(4) {
            if p[3] == 0 {
                p.fill(0);
            }
        }
    }
    frames
}

fn ours(bytes: &[u8]) -> Vec<Vec<u8>> {
    let (v, _) = CodecSet::pinned()
        .decode(
            &mut Cursor::new(bytes.to_vec()),
            "aseprite",
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
        .unwrap();
    clear_invisible(match v {
        Value::Image(i) => vec![to_rgba8_rows(&i.px).unwrap()],
        Value::Frames(f) => f
            .frames()
            .iter()
            .map(|x| to_rgba8_rows(&x.px).unwrap())
            .collect(),
        Value::Audio(_) => panic!(),
    })
}

fn reference(bytes: &[u8]) -> Vec<Vec<u8>> {
    let ase = asefile::AsepriteFile::read(Cursor::new(bytes)).unwrap();
    clear_invisible(
        (0..ase.num_frames())
            .map(|f| ase.frame(f).image().into_raw())
            .collect(),
    )
}

fn random_sprite(seed: u64) -> FileGen {
    let mut r = kura_engine::determinism::rng(seed);
    let mut pick = |n: u32| r.next_u32() % n;
    let depth = [32u16, 16, 8][pick(3) as usize];
    let (w, h) = (4 + pick(20) as u16, 4 + pick(16) as u16);
    let n_layers = 1 + pick(5) as usize;
    let layers: Vec<AseLayer> = (0..n_layers)
        .map(|_| AseLayer {
            // 見える層がほとんど、ときどき見えない層
            flags: if pick(6) == 0 { 0 } else { 1 },
            kind: 0,
            level: 0,
            blend: pick(19) as u16,
            opacity: [255u8, 255, 128, 37, 0][pick(5) as usize],
        })
        .collect();
    let n_frames = 1 + pick(3) as usize;
    let mut frames = Vec::new();
    for k in 0..n_frames {
        let mut cels = Vec::new();
        for l in 0..n_layers {
            if pick(5) == 0 {
                continue;
            }
            let kind = if k > 0 && pick(4) == 0 {
                1
            } else {
                [0u16, 2][pick(2) as usize]
            };
            cels.push(AseCel {
                layer: l as u16,
                x: pick(12) as i16 - 4,
                y: pick(10) as i16 - 3,
                opacity: [255u8, 200, 64][pick(3) as usize],
                z: 0,
                kind,
                w: 1 + pick(18) as u16,
                h: 1 + pick(14) as u16,
                link: 0,
                flip: false,
            });
        }
        frames.push(AseFrame {
            duration: 10 + pick(200) as u16,
            cels,
        });
    }
    // リンクは、指す先（フレーム 0）に同じ層のセルがあるときだけ残す
    let first: Vec<u16> = frames[0].cels.iter().map(|c| c.layer).collect();
    for f in frames.iter_mut().skip(1) {
        for c in &mut f.cels {
            if c.kind == 1 && !first.contains(&c.layer) {
                c.kind = 2;
            }
        }
    }
    FileGen::Aseprite {
        w,
        h,
        depth,
        flags: 1,
        transparent: pick(4) as u8,
        palette_len: 4 + pick(30) as u16,
        layers,
        frames,
        tileset: None,
        tags: vec![],
        slices: vec![],
        seed,
        opaque_palette: false,
    }
}

#[test]
fn compositing_matches_asefile() {
    for seed in 0..300u64 {
        let g = random_sprite(seed);
        let bytes = g.bytes().unwrap();
        assert_eq!(ours(&bytes), reference(&bytes), "seed {seed}: {g:?}");
    }
}

/// タイルマップの層（反転なし）も同じ。
#[test]
fn tilemaps_match_asefile() {
    for (depth, seed) in [(32u16, 1u64), (16, 2), (8, 3)] {
        let g = FileGen::Aseprite {
            w: 20,
            h: 16,
            depth,
            flags: 1,
            transparent: 0,
            palette_len: 16,
            layers: vec![
                AseLayer {
                    flags: 1,
                    kind: 0,
                    level: 0,
                    blend: 0,
                    opacity: 255,
                },
                AseLayer {
                    flags: 1,
                    kind: 2,
                    level: 0,
                    blend: 1,
                    opacity: 200,
                },
            ],
            frames: vec![AseFrame {
                duration: 100,
                cels: vec![
                    AseCel {
                        layer: 0,
                        x: 0,
                        y: 0,
                        opacity: 255,
                        z: 0,
                        kind: 2,
                        w: 20,
                        h: 16,
                        link: 0,
                        flip: false,
                    },
                    AseCel {
                        layer: 1,
                        x: 2,
                        y: -1,
                        opacity: 255,
                        z: 0,
                        kind: 3,
                        w: 4,
                        h: 3,
                        link: 0,
                        flip: false,
                    },
                ],
            }],
            tileset: Some((5, 6, 4)),
            tags: vec![],
            slices: vec![],
            seed,
            opaque_palette: false,
        };
        let bytes = g.bytes().unwrap();
        assert_eq!(ours(&bytes), reference(&bytes), "{depth}");
    }
}

/// 番号のまま重ねる経路（パレットが不透明・通常・不透明度 255）は Indexed8 になり、広げると asefile と同じ。
#[test]
fn index_mode_matches_asefile() {
    let mut indexed = 0;
    for seed in 0..120u64 {
        let mut g = random_sprite(seed);
        if let FileGen::Aseprite {
            depth,
            layers,
            frames,
            opaque_palette,
            ..
        } = &mut g
        {
            *depth = 8;
            *opaque_palette = true;
            for l in layers.iter_mut() {
                l.blend = 0;
                l.opacity = 255;
            }
            for f in frames.iter_mut() {
                for c in &mut f.cels {
                    c.opacity = 255;
                }
            }
        }
        let bytes = g.bytes().unwrap();
        let (v, _) = CodecSet::pinned()
            .decode(
                &mut Cursor::new(bytes.clone()),
                "aseprite",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        let fmt = match &v {
            Value::Image(i) => i.px.format(),
            Value::Frames(f) => f.frames()[0].px.format(),
            Value::Audio(_) => panic!(),
        };
        if fmt == kura_engine::value::PixelFormat::Indexed8 {
            indexed += 1;
        }
        assert_eq!(ours(&bytes), reference(&bytes), "seed {seed}");
    }
    assert_eq!(indexed, 120);
}

/// グループ（別に合成しない）と見えない親のグループも、asefile と同じ。
#[test]
fn groups_match_asefile() {
    for seed in 0..120u64 {
        let mut r = kura_engine::determinism::rng(1_000 + seed);
        let mut pick = |n: u32| r.next_u32() % n;
        let mut layers = vec![AseLayer {
            flags: 1,
            kind: 0,
            level: 0,
            blend: pick(19) as u16,
            opacity: 255,
        }];
        // グループ 1 つ（見えないこともある）と、その子 2 つ、その後ろにもう 1 つ
        layers.push(AseLayer {
            flags: if pick(3) == 0 { 0 } else { 1 },
            kind: 1,
            level: 0,
            blend: 0,
            opacity: 255,
        });
        layers.push(AseLayer {
            flags: 1,
            kind: 0,
            level: 1,
            blend: pick(19) as u16,
            opacity: [255u8, 128][pick(2) as usize],
        });
        layers.push(AseLayer {
            flags: 1,
            kind: 0,
            level: 1,
            blend: pick(19) as u16,
            opacity: 255,
        });
        layers.push(AseLayer {
            flags: 1,
            kind: 0,
            level: 0,
            blend: pick(19) as u16,
            opacity: 200,
        });
        let cels: Vec<AseCel> = [0u16, 2, 3, 4]
            .into_iter()
            .map(|l| AseCel {
                layer: l,
                x: pick(6) as i16 - 2,
                y: pick(6) as i16 - 2,
                opacity: [255u8, 180][pick(2) as usize],
                z: 0,
                kind: 2,
                w: 3 + pick(10) as u16,
                h: 3 + pick(10) as u16,
                link: 0,
                flip: false,
            })
            .collect();
        let g = FileGen::Aseprite {
            w: 10,
            h: 9,
            depth: [32u16, 16, 8][pick(3) as usize],
            flags: 1,
            transparent: 0,
            palette_len: 16,
            layers,
            frames: vec![AseFrame { duration: 50, cels }],
            tileset: None,
            tags: vec![],
            slices: vec![],
            seed,
            opaque_palette: false,
        };
        let bytes = g.bytes().unwrap();
        assert_eq!(ours(&bytes), reference(&bytes), "seed {seed}: {g:?}");
    }
}
