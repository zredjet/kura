//! バイトの読み口（読み込んだバイト列か、シークできる読み口を同じ形で読む。M16 段 1 の FLAC・段 2 の Ogg）。
//!
//! 読み口は [`CHUNK`] ずつ補い、補うたびに中止を見る。[`Input::mark`] で残した位置より前は、補うときにも捨てない
//! （フレームの CRC を終わりにまとめて求める・区切りを先まで見る、ため）。

use std::io::{Read, Seek, SeekFrom};

use crate::codec::ReadSeek;
use crate::codec::common::Media;
use crate::error::EngineError;
use crate::monitor::Monitor;

/// 読み口から 1 度に読む量。
const CHUNK: usize = 64 << 10;

enum Backing<'a> {
    Bytes(&'a [u8]),
    Reader(&'a mut (dyn ReadSeek + Send)),
}

/// バイトの読み口（読み込んだバイト列か、シークできる読み口。読み口は [`CHUNK`] ずつ補い、補うたびに中止を見る）。
pub(crate) struct Input<'a> {
    backing: Backing<'a>,
    /// 読み口の緩衝（`Reader` のとき）。
    buf: Vec<u8>,
    /// `buf[0]` のファイルの位置（`Reader` のとき）。
    base: u64,
    pub pos: usize,
    pub end: usize,
    eof: bool,
    /// 補うときに捨てずに残す位置（読んでいるフレームの頭。CRC-16 をフレームの終わりにまとめて求めるため）。
    pub keep: Option<usize>,
    pub mon: &'a dyn Monitor,
    /// 切れたファイルの誤りに付けるコーデックの識別子。
    codec: &'static str,
}

impl<'a> Input<'a> {
    /// ファイルが途中で切れているときの誤り。
    pub fn truncated(&self) -> EngineError {
        EngineError::Decode {
            codec: self.codec.to_string(),
            message: "ファイルが途中で切れています".into(),
        }
    }

    /// 読み込んだバイト列か読み口を頭から読む（読み口は頭へ戻す）。`codec` は切れたファイルの誤りに付ける識別子。
    pub fn new(
        media: &'a mut Media<'_>,
        mon: &'a dyn Monitor,
        codec: &'static str,
    ) -> Result<Self, EngineError> {
        let (backing, end, eof) = match media {
            Media::Bytes(data) => (Backing::Bytes(data), data.len(), true),
            Media::Reader(r) => {
                r.seek(SeekFrom::Start(0))?;
                (Backing::Reader(&mut **r), 0, false)
            }
        };
        Ok(Self {
            backing,
            buf: Vec::new(),
            base: 0,
            pos: 0,
            end,
            eof,
            keep: None,
            mon,
            codec,
        })
    }

    pub fn data(&self) -> &[u8] {
        match &self.backing {
            Backing::Bytes(d) => d,
            Backing::Reader(_) => &self.buf[..self.end],
        }
    }

    /// 読み口から補う（読めたら true。終わり・読み込んだバイト列なら false）。
    pub fn refill(&mut self) -> Result<bool, EngineError> {
        if self.eof {
            return Ok(false);
        }
        if self.mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let Backing::Reader(r) = &mut self.backing else {
            return Ok(false);
        };
        // 読み終えた分（読んでいるフレームの頭より前）を捨てて前へ詰める
        let from = self.keep.map_or(self.pos, |k| k.min(self.pos));
        if from > 0 {
            self.buf.copy_within(from..self.end, 0);
            self.base += from as u64;
            self.end -= from;
            self.pos -= from;
            self.keep = self.keep.map(|k| k - from);
        }
        let want = self.end + CHUNK;
        if self.buf.len() < want {
            self.buf
                .try_reserve_exact(want - self.buf.len())
                .map_err(|_| EngineError::Limit("読み口の領域を確保できません".into()))?;
            self.buf.resize(want, 0);
        }
        let n = loop {
            match r.read(&mut self.buf[self.end..want]) {
                Ok(n) => break n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        };
        if n == 0 {
            self.eof = true;
            return Ok(false);
        }
        self.end += n;
        Ok(true)
    }

    /// 次の 1 バイト（終わりなら None）。
    #[inline]
    pub fn byte(&mut self) -> Result<Option<u8>, EngineError> {
        if self.pos >= self.end && !self.refill()? {
            return Ok(None);
        }
        let b = self.data()[self.pos];
        self.pos += 1;
        Ok(Some(b))
    }

    /// 次の `n` バイトまでを読まずに見る（終わりが近ければ短い）。
    pub fn peek(&mut self, n: usize) -> Result<&[u8], EngineError> {
        while self.end - self.pos < n && self.refill()? {}
        let end = self.end.min(self.pos + n);
        let pos = self.pos;
        Ok(&self.data()[pos..end])
    }

    pub fn advance(&mut self, n: usize) {
        self.pos = (self.pos + n).min(self.end);
    }

    /// 今の位置をフレームの頭として残す（[`Input::crc16_since_mark`] まで捨てない）。
    pub fn mark(&mut self) {
        self.keep = Some(self.pos);
    }

    /// 残した頭から今の位置までのバイト数。
    pub fn since_mark(&self) -> usize {
        self.keep.map_or(0, |k| self.pos - k)
    }

    /// 残した頭から `rel` バイトまでを読み口から補う（終わりまでに届かなければ false）。
    pub fn ensure_rel(&mut self, rel: usize) -> Result<bool, EngineError> {
        loop {
            let k = self.keep.unwrap_or(self.pos);
            if self.end - k >= rel {
                return Ok(true);
            }
            if !self.refill()? {
                return Ok(false);
            }
        }
    }

    /// 残した頭から読み口の終わりまでのバイト数。
    pub fn avail_rel(&self) -> usize {
        self.end - self.keep.unwrap_or(self.pos)
    }

    /// 残した頭からの `a..b` のバイト列。
    pub fn slice_rel(&self, a: usize, b: usize) -> &[u8] {
        let k = self.keep.unwrap_or(self.pos);
        &self.data()[k + a..k + b]
    }

    /// 位置を、残した頭から `rel` バイトの所にする（残すのをやめる）。
    pub fn release_at(&mut self, rel: usize) {
        let k = self.keep.take().unwrap_or(self.pos);
        self.pos = k + rel;
    }

    /// ちょうど `out.len()` バイトを読む（足りなければ壊れたファイル）。
    pub fn read_exact(&mut self, out: &mut [u8]) -> Result<(), EngineError> {
        let mut done = 0;
        while done < out.len() {
            if self.pos >= self.end && !self.refill()? {
                return Err(self.truncated());
            }
            let n = (self.end - self.pos).min(out.len() - done);
            let pos = self.pos;
            out[done..done + n].copy_from_slice(&self.data()[pos..pos + n]);
            self.pos += n;
            done += n;
        }
        Ok(())
    }

    pub fn read_u8(&mut self) -> Result<u8, EngineError> {
        match self.byte()? {
            Some(b) => Ok(b),
            None => Err(self.truncated()),
        }
    }

    pub fn be24(&mut self) -> Result<u32, EngineError> {
        let mut b = [0u8; 3];
        self.read_exact(&mut b)?;
        Ok(u32::from_be_bytes([0, b[0], b[1], b[2]]))
    }

    /// `n` バイトを読み飛ばす（読み口なら、緩衝の外はシークで）。終わりを越えたら、次に読むところで切れたファイルになる。
    pub fn skip(&mut self, n: u64) -> Result<(), EngineError> {
        let avail = (self.end - self.pos) as u64;
        if n <= avail {
            self.pos += n as usize;
            return Ok(());
        }
        match &mut self.backing {
            Backing::Bytes(_) => {
                self.pos = self.end;
                Ok(())
            }
            Backing::Reader(r) => {
                let target = (self.base + self.pos as u64).saturating_add(n);
                r.seek(SeekFrom::Start(target))?;
                self.base = target;
                self.pos = 0;
                self.end = 0;
                self.eof = false;
                Ok(())
            }
        }
    }
}
