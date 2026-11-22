// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]
// The following lints are allowed in all Symphonia crates. Please see clippy.toml for their
// justification.
#![allow(clippy::comparison_chain)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::identity_op)]
#![allow(clippy::manual_range_contains)]
// TODO: Remove this when refactoring AAC.
#![allow(clippy::needless_range_loop)]

mod aac;
mod adts;
// kura：OS の libm に頼らない f32 の超越関数（KURA.md）
mod kura_math;

pub use aac::AacDecoder;
pub use adts::AdtsReader;

/// kura: marks this crate as kura's patched copy (see `KURA.md`; not present in the crates.io release).
/// kura-engine refers to it, so the build fails if the upstream crate is linked instead of this copy
/// (for example when a `[patch.crates-io]` entry is missing or no longer matches the required version).
pub const KURA_PATCHED: u32 = 1;
