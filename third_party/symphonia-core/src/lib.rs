// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
#![forbid(unsafe_code)]
// The following lints are allowed in all Symphonia crates. Please see clippy.toml for their
// justification.
#![allow(clippy::comparison_chain)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::identity_op)]
#![allow(clippy::manual_range_contains)]

pub mod audio;
pub mod checksum;
pub mod codecs;
pub mod common;
pub mod dsp;
pub mod errors;
pub mod formats;
pub mod io;
pub mod meta;
pub mod packet;
pub mod subtitle;
pub mod units;
pub mod util;
pub mod video;

/// kura: marks this crate as kura's patched copy (see `KURA.md`; not present in the crates.io release).
/// kura-engine refers to it, so the build fails if the upstream crate is linked instead of this copy
/// (for example when a `[patch.crates-io]` entry is missing or no longer matches the required version).
pub const KURA_PATCHED: u32 = 1;
