//! 時間の写しの連鎖（M13 段 4。純粋な関数）：`Plan::time_chain` の `TimeLink` の列を順・逆にたどる・元への写しの区分。
//!
//! - [`forward`]・[`forward_end`]：連鎖を順に写す（入力 → 出力）。区間の終わりは `_end`（`Segments` の終わりちょうども写す）。
//! - [`backward`]・[`backward_end`]：逆に写す（出力 → 入力）。`Rate` の往復は丸めの分（from/(2·to) + 1/2 フレーム）までずれうる。
//! - [`forward_line`]：区間の出力に写る部分の写し（1 本の直線）を延ばす（フェードの見かけの始まり・終わり。符号つき）。
//! - [`source_map`]：出力のフレームを元（入力 0）のフレームへ写す区分の列。意味は
//!   `in = in_start + round((out − out_start) × in_rate / out_rate)`。`Unknown`・`Reset` があれば None。ふつうは 1 区分。

use kura_engine::Shape;
use kura_engine::geom::div_round_half_even;
use kura_engine::mapping::TimeMap;
use kura_engine::render::TimeLink;

/// 元への写しの 1 区分（出力の `[out_start, out_end)` が、元の `in_start` から `in_rate / out_rate` の比で並ぶ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePiece {
    pub out_start: u64,
    pub out_end: u64,
    pub in_start: u64,
    pub in_rate: u32,
    pub out_rate: u32,
}

/// 形の (rate, frames)（長さの分からないものは None）。
pub(crate) fn rate_frames(s: &Shape) -> Option<(u32, u64)> {
    match *s {
        Shape::Audio {
            rate,
            frames: kura_engine::Dim::Exact(frames),
            ..
        } => Some((rate, frames)),
        _ => None,
    }
}

/// 位置を連鎖の順に写す。
pub fn forward(links: &[TimeLink], pos: u64) -> Option<u64> {
    links.iter().try_fold(pos, |p, l| l.time.pos(p))
}

/// 区間の終わりを連鎖の順に写す。
pub fn forward_end(links: &[TimeLink], end: u64) -> Option<u64> {
    links.iter().try_fold(end, |p, l| l.time.pos_end(p))
}

/// 位置を連鎖の逆に写す。
pub fn backward(links: &[TimeLink], pos: u64) -> Option<u64> {
    links.iter().rev().try_fold(pos, |p, l| l.time.back(p))
}

/// 区間の終わりを連鎖の逆に写す。
pub fn backward_end(links: &[TimeLink], end: u64) -> Option<u64> {
    links.iter().rev().try_fold(end, |p, l| l.time.back_end(p))
}

fn gcd(mut a: i128, mut b: i128) -> i128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.abs()
}

/// 区間 `[lo, hi)`（連鎖の入力のフレーム）の出力に写る部分が 1 本の直線に収まるとき、その直線を延ばして入力の位置 `at` を
/// 写した出力の位置（符号つき。出力の頭より前なら負、出力の終わりより後もある）。フェードの見かけの始まり・終わりに使う。
///
/// 直線は区間が通る区分のずらし（`Segments`）とレートの比（`Rate`）を重ねた有理数の式で持ち、最後に 1 回 round half even で
/// 丸める（`Rate` の写しと同じ丸め）。区間の出力に写る部分が `Segments` の 2 つ以上の区分にかかる（継ぎ目）・どれにも
/// かからない・空、`Unknown`・`Reset`・あふれは None。
pub fn forward_line(links: &[TimeLink], range: (u64, u64), at: u64) -> Option<i64> {
    let (mut lo, mut hi) = range;
    // 出力 = (入力 · num + add) / den
    let (mut num, mut add, mut den) = (1i128, 0i128, 1i128);
    for link in links {
        if lo >= hi {
            return None;
        }
        match &link.time {
            TimeMap::Identity => {}
            TimeMap::Segments { segs } => {
                let mut hit = segs.iter().filter(|s| s.src_start < hi && lo < s.src_end);
                let s = hit.next()?;
                if hit.next().is_some() {
                    return None;
                }
                let shift = i128::from(s.dst_start) - i128::from(s.src_start);
                add = add.checked_add(shift.checked_mul(den)?)?;
                lo = s.dst_start.checked_add(lo.max(s.src_start) - s.src_start)?;
                hi = s.dst_start.checked_add(hi.min(s.src_end) - s.src_start)?;
            }
            TimeMap::Rate { from, to } => {
                lo = link.time.pos(lo)?;
                hi = link.time.pos_end(hi)?;
                num = num.checked_mul(i128::from(*to))?;
                add = add.checked_mul(i128::from(*to))?;
                den = den.checked_mul(i128::from(*from))?;
                let g = gcd(gcd(num, add), den);
                if g > 1 {
                    (num, add, den) = (num / g, add / g, den / g);
                }
            }
            TimeMap::Unknown | TimeMap::Reset => return None,
        }
    }
    if lo >= hi {
        return None;
    }
    div_round_half_even(i128::from(at).checked_mul(num)?.checked_add(add)?, den)
}

/// 区分の中の位置 `out` の元の位置。
fn in_of(p: &SourcePiece, out: u64) -> Option<u64> {
    let d = i128::from(out.checked_sub(p.out_start)?);
    let off = div_round_half_even(d * i128::from(p.in_rate), i128::from(p.out_rate.max(1)))?;
    p.in_start.checked_add(u64::try_from(off).ok()?)
}

/// 元への写しの区分の列（モジュールの説明）。連鎖が空なら入力の全体の 1 区分。
pub fn source_map(links: &[TimeLink], input: &Shape) -> Option<Vec<SourcePiece>> {
    let (in_rate, frames) = rate_frames(input)?;
    let mut pieces = vec![SourcePiece {
        out_start: 0,
        out_end: frames,
        in_start: 0,
        in_rate,
        out_rate: in_rate,
    }];
    for link in links {
        let out_rate = rate_frames(&link.output)?.0;
        pieces = match &link.time {
            TimeMap::Identity => pieces
                .into_iter()
                .map(|p| SourcePiece { out_rate, ..p })
                .collect(),
            TimeMap::Segments { segs } => {
                let mut next = Vec::new();
                for p in &pieces {
                    for s in segs {
                        let (a, b) = (p.out_start.max(s.src_start), p.out_end.min(s.src_end));
                        if a >= b {
                            continue;
                        }
                        next.push(SourcePiece {
                            out_start: s.dst_start.checked_add(a - s.src_start)?,
                            out_end: s.dst_start.checked_add(b - s.src_start)?,
                            in_start: in_of(p, a)?,
                            in_rate,
                            out_rate,
                        });
                    }
                }
                next
            }
            TimeMap::Rate { .. } => pieces
                .iter()
                .map(|p| {
                    Some(SourcePiece {
                        out_start: link.time.pos(p.out_start)?,
                        out_end: link.time.pos_end(p.out_end)?,
                        in_start: p.in_start,
                        in_rate,
                        out_rate,
                    })
                })
                .collect::<Option<Vec<_>>>()?,
            TimeMap::Unknown | TimeMap::Reset => return None,
        };
        pieces.retain(|p| p.out_start < p.out_end);
    }
    pieces.sort_by_key(|p| p.out_start);
    Some(pieces)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kura_engine::Dim;
    use kura_engine::doc::NodeId;
    use kura_engine::mapping::Seg;

    fn audio(rate: u32, frames: u64) -> Shape {
        Shape::Audio {
            rate,
            channels: 1,
            frames: Dim::Exact(frames),
        }
    }

    fn link(time: TimeMap, input: Shape, output: Shape) -> TimeLink {
        TimeLink {
            node: NodeId(1),
            op: "x".into(),
            enabled: true,
            time,
            input,
            output,
        }
    }

    #[test]
    fn trim_then_resample_maps_to_the_source() {
        // 入力 48 kHz・1,000 フレーム → trim [100, 900) → 24 kHz
        let trim = link(
            TimeMap::Segments {
                segs: vec![Seg {
                    src_start: 100,
                    src_end: 900,
                    dst_start: 0,
                }],
            },
            audio(48_000, 1_000),
            audio(48_000, 800),
        );
        let rs = link(
            TimeMap::Rate {
                from: 48_000,
                to: 24_000,
            },
            audio(48_000, 800),
            audio(24_000, 400),
        );
        let links = vec![trim, rs];
        let map = source_map(&links, &audio(48_000, 1_000)).unwrap();
        assert_eq!(
            map,
            vec![SourcePiece {
                out_start: 0,
                out_end: 400,
                in_start: 100,
                in_rate: 48_000,
                out_rate: 24_000
            }]
        );
        assert_eq!(in_of(&map[0], 10), Some(120));
        assert_eq!(forward(&links, 300), Some(100));
        assert_eq!(backward(&links, 100), Some(300));
        assert_eq!(forward_end(&links, 900), Some(400));
        assert_eq!(backward_end(&links, 400), Some(900));
        assert_eq!(forward(&links, 50), None, "trim の外");
        // 空の連鎖は全体の 1 区分
        assert_eq!(
            source_map(&[], &audio(8_000, 10)).unwrap(),
            vec![SourcePiece {
                out_start: 0,
                out_end: 10,
                in_start: 0,
                in_rate: 8_000,
                out_rate: 8_000
            }]
        );
        let unknown = link(TimeMap::Unknown, audio(8_000, 10), audio(8_000, 10));
        assert_eq!(source_map(&[unknown], &audio(8_000, 10)), None);
    }

    fn trim(start: u64, end: u64, frames: u64) -> TimeLink {
        link(
            TimeMap::Segments {
                segs: vec![Seg {
                    src_start: start,
                    src_end: end,
                    dst_start: 0,
                }],
            },
            audio(8_000, frames),
            audio(8_000, end - start),
        )
    }

    fn rate(from: u32, to: u32, frames: u64, out: u64) -> TimeLink {
        link(
            TimeMap::Rate { from, to },
            audio(from, frames),
            audio(to, out),
        )
    }

    /// 区間の出力に写る部分の直線を延ばす：下流がなければそのまま、trim で頭を切ると負、レートは比を掛け、最後に 1 回
    /// round half even で丸める。
    #[test]
    fn forward_line_extends_the_mapping_of_the_visible_part() {
        // 下流がない：そのまま
        assert_eq!(forward_line(&[], (0, 3_000), 0), Some(0));
        assert_eq!(forward_line(&[], (7_000, 10_000), 10_000), Some(10_000));
        // trim [1,000, 9,000)：フェードイン [0, 3,000) の見かけの始まりは −1,000、終わりの延長は入力の終わりの先
        let t = [trim(1_000, 9_000, 10_000)];
        assert_eq!(forward_line(&t, (0, 3_000), 0), Some(-1_000));
        assert_eq!(forward_line(&t, (0, 3_000), 3_000), Some(2_000));
        assert_eq!(forward_line(&t, (7_000, 10_000), 10_000), Some(9_000));
        // trim のあとに 8 → 16 kHz：比を掛ける
        let tr = [
            trim(1_000, 9_000, 10_000),
            rate(8_000, 16_000, 8_000, 16_000),
        ];
        assert_eq!(forward_line(&tr, (0, 3_000), 0), Some(-2_000));
        assert_eq!(forward_line(&tr, (7_000, 10_000), 10_000), Some(18_000));
        // 先に 3 → 2 のレート、そのあと trim [1, …)：直線は (2x − 3) / 3 で、x = 0 は −1
        let rt = [
            link(
                TimeMap::Rate { from: 3, to: 2 },
                audio(3, 300),
                audio(2, 200),
            ),
            link(
                TimeMap::Segments {
                    segs: vec![Seg {
                        src_start: 1,
                        src_end: 200,
                        dst_start: 0,
                    }],
                },
                audio(2, 200),
                audio(2, 199),
            ),
        ];
        assert_eq!(forward_line(&rt, (0, 30), 0), Some(-1));
        // 4 → 8 kHz・trim [1, …)・8 → 4 kHz：直線は (2x − 1) / 2。半分ちょうどは偶数へ（x = 0 は −1/2 → 0）
        let half = [
            rate(4_000, 8_000, 10, 20),
            link(
                TimeMap::Segments {
                    segs: vec![Seg {
                        src_start: 1,
                        src_end: 20,
                        dst_start: 0,
                    }],
                },
                audio(8_000, 20),
                audio(8_000, 19),
            ),
            rate(8_000, 4_000, 19, 10),
        ];
        assert_eq!(forward_line(&half, (0, 5), 0), Some(0));
        assert_eq!(
            forward_line(&half, (0, 5), 3),
            Some(2),
            "(6 − 1) / 2 = 2.5 → 2"
        );
    }

    /// 区間の出力に写る部分が 1 本の直線で写せない（区分の継ぎ目にかかる・どこにも写らない）、写しが分からない、あふれは None。
    #[test]
    fn forward_line_refuses_seams_and_unknown_maps() {
        // 区間 [0, 3,000) が 2 つの区分にかかる（真ん中を抜いて継ぐ）
        let cut = link(
            TimeMap::Segments {
                segs: vec![
                    Seg {
                        src_start: 0,
                        src_end: 1_000,
                        dst_start: 0,
                    },
                    Seg {
                        src_start: 2_000,
                        src_end: 10_000,
                        dst_start: 1_000,
                    },
                ],
            },
            audio(8_000, 10_000),
            audio(8_000, 9_000),
        );
        assert_eq!(
            forward_line(std::slice::from_ref(&cut), (0, 3_000), 0),
            None
        );
        // 継ぎ目の後ろだけにかかる区間は 1 本で写せる
        assert_eq!(forward_line(&[cut], (7_000, 10_000), 10_000), Some(9_000));
        // どこにも写らない（trim の外）
        assert_eq!(
            forward_line(&[trim(5_000, 9_000, 10_000)], (0, 3_000), 0),
            None
        );
        // 区間が空
        assert_eq!(forward_line(&[], (0, 0), 0), None);
        // 写しが分からない
        for time in [TimeMap::Unknown, TimeMap::Reset] {
            let l = link(time, audio(8_000, 10), audio(8_000, 10));
            assert_eq!(forward_line(&[l], (0, 5), 0), None);
        }
        // あふれ（i64 に収まらない）
        let big = [rate(1, u32::MAX, 10, 10)];
        assert_eq!(forward_line(&big, (0, 10), u64::MAX), None);
    }

    #[test]
    fn segments_can_split_into_several_pieces() {
        // [0,10) と [20,30) を並べる
        let cut = link(
            TimeMap::Segments {
                segs: vec![
                    Seg {
                        src_start: 0,
                        src_end: 10,
                        dst_start: 0,
                    },
                    Seg {
                        src_start: 20,
                        src_end: 30,
                        dst_start: 10,
                    },
                ],
            },
            audio(8_000, 40),
            audio(8_000, 20),
        );
        let map = source_map(&[cut], &audio(8_000, 40)).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(
            (map[1].out_start, map[1].out_end, map[1].in_start),
            (10, 20, 20)
        );
    }
}
