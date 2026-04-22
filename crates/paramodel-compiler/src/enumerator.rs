// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Mixed-radix trial enumeration (SRD-0010 §1).
//!
//! Given an ordered list of axes, the enumerator translates between
//! dense trial indices and offset vectors, and produces reducto-shaped
//! trial codes. The encoding is:
//!
//! - Compute each axis's cardinality.
//! - Stride for rank `i` is the product of cardinalities at higher
//!   ranks (`stride[N-1] = 1`).
//! - Offset for rank `i` at trial `T` is `(T / stride[i]) mod C_i`.
//! - Reverse: `T = Σ offset[i] × stride[i]`.
//!
//! Trial-code digit width is 4 bits if the largest axis cardinality
//! is `≤ 16`; otherwise 8 bits. See §1.4 for worked examples.

use paramodel_plan::Axis;

/// Enumerates trial indices against an ordered axis stack.
#[derive(Debug, Clone)]
pub struct MixedRadixEnumerator {
    cardinalities: Vec<u32>,
    strides:       Vec<u64>,
    trial_count:   u64,
    digit_bits:    u8,
}

impl MixedRadixEnumerator {
    /// Build from the plan's axis list (authored order).
    ///
    /// An empty axis list yields a single "trial 0" enumeration;
    /// reducto treats a plan with no axes as one trial running the
    /// authored configuration.
    #[must_use]
    pub fn new(axes: &[Axis]) -> Self {
        let cardinalities: Vec<u32> = axes
            .iter()
            .map(|a| u32::try_from(a.cardinality()).unwrap_or(u32::MAX))
            .collect();
        let trial_count = cardinalities
            .iter()
            .map(|c| u64::from(*c))
            .product::<u64>()
            .max(1);
        let strides = compute_strides(&cardinalities);
        let digit_bits = if cardinalities.iter().max().copied().unwrap_or(1) <= 16 {
            4
        } else {
            8
        };
        Self {
            cardinalities,
            strides,
            trial_count,
            digit_bits,
        }
    }

    /// Total number of trials (product of axis cardinalities; 1 when
    /// no axes).
    #[must_use]
    pub const fn trial_count(&self) -> u64 {
        self.trial_count
    }

    /// Number of authored axes.
    #[must_use]
    pub const fn axis_count(&self) -> usize {
        self.cardinalities.len()
    }

    /// Cardinalities in authored order.
    #[must_use]
    pub fn cardinalities(&self) -> &[u32] {
        &self.cardinalities
    }

    /// Strides in authored order.
    #[must_use]
    pub fn strides(&self) -> &[u64] {
        &self.strides
    }

    /// Digit-width in bits (4 or 8) used by [`Self::trial_code`].
    #[must_use]
    pub const fn digit_bits(&self) -> u8 {
        self.digit_bits
    }

    /// Offsets for a given trial index. The returned vector is length
    /// `axis_count()`; axes with cardinality 1 always contribute
    /// `0`.
    #[must_use]
    pub fn offsets(&self, trial_index: u64) -> Vec<u32> {
        assert!(
            trial_index < self.trial_count,
            "trial_index {trial_index} out of range ({} trials)",
            self.trial_count,
        );
        self.cardinalities
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if *c == 0 {
                    0
                } else {
                    u32::try_from((trial_index / self.strides[i]) % u64::from(*c))
                        .unwrap_or(u32::MAX)
                }
            })
            .collect()
    }

    /// Inverse of [`Self::offsets`].
    #[must_use]
    pub fn trial_index(&self, offsets: &[u32]) -> u64 {
        offsets
            .iter()
            .zip(self.strides.iter())
            .map(|(o, s)| u64::from(*o) * s)
            .sum()
    }

    /// Reducto trial code: `"0x…"` with digits sized by
    /// [`Self::digit_bits`].
    #[must_use]
    pub fn trial_code(&self, trial_index: u64) -> String {
        let offsets = self.offsets(trial_index);
        let mut out = String::from("0x");
        for o in &offsets {
            if self.digit_bits == 4 {
                let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{o:x}"));
            } else {
                let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{o:02x}"));
            }
        }
        out
    }
}

fn compute_strides(cardinalities: &[u32]) -> Vec<u64> {
    let n = cardinalities.len();
    if n == 0 {
        return Vec::new();
    }
    let mut strides = vec![1u64; n];
    for i in (0..n.saturating_sub(1)).rev() {
        strides[i] = strides[i + 1] * u64::from(cardinalities[i + 1]);
    }
    strides
}

#[cfg(test)]
mod tests {
    use paramodel_elements::{ElementName, ParameterName, Value};
    use paramodel_plan::{AxisName, ElementParameterRef};

    use super::*;

    fn axis(name: &str, element: &str, param: &str, values: Vec<i64>) -> Axis {
        Axis::builder()
            .name(AxisName::new(name).unwrap())
            .target(ElementParameterRef::new(
                ElementName::new(element).unwrap(),
                ParameterName::new(param).unwrap(),
            ))
            .values(
                values
                    .into_iter()
                    .map(|v| Value::integer(ParameterName::new(param).unwrap(), v, None))
                    .collect(),
            )
            .build()
    }

    // ---------- SRD §1.4 worked examples ----------

    #[test]
    fn zero_axes_yields_one_trial() {
        let e = MixedRadixEnumerator::new(&[]);
        assert_eq!(e.trial_count(), 1);
        assert_eq!(e.offsets(0), Vec::<u32>::new());
        assert_eq!(e.trial_code(0), "0x");
    }

    #[test]
    fn three_small_axes_4bit_digits() {
        // axes = a:[1,2,3]  b:[asm,dra,ghi]  c:[yo]
        //   Trial 4 → offsets [1, 1, 0] → code "0x110"
        let axes = vec![
            axis("a", "x", "a", vec![1, 2, 3]),
            axis("b", "x", "b", vec![1, 2, 3]),
            axis("c", "x", "c", vec![1]),
        ];
        let e = MixedRadixEnumerator::new(&axes);
        assert_eq!(e.trial_count(), 9);
        assert_eq!(e.digit_bits(), 4);
        let offsets = e.offsets(4);
        assert_eq!(offsets, vec![1, 1, 0]);
        assert_eq!(e.trial_code(4), "0x110");
        assert_eq!(e.trial_index(&offsets), 4);
    }

    #[test]
    fn three_axes_varying_cardinality_4bit() {
        // axes = v1:[a,b,c]  v2:[u,v]  v3:[w,x,y,z]
        //   Trial 10 → offsets [1, 0, 2] → code "0x102"
        let axes = vec![
            axis("v1", "x", "p1", vec![1, 2, 3]),
            axis("v2", "x", "p2", vec![1, 2]),
            axis("v3", "x", "p3", vec![1, 2, 3, 4]),
        ];
        let e = MixedRadixEnumerator::new(&axes);
        assert_eq!(e.trial_count(), 24);
        assert_eq!(e.digit_bits(), 4);
        let offsets = e.offsets(10);
        assert_eq!(offsets, vec![1, 0, 2]);
        assert_eq!(e.trial_code(10), "0x102");
    }

    #[test]
    fn wide_axis_switches_to_8bit_digits() {
        // axes = a:[0..17]  b:[what, up]
        //   Trial 37 → offsets [2, 0] → code "0x0200" (widened 8-bit)
        let axes = vec![
            axis("a", "x", "a", (0..17).collect()),
            axis("b", "x", "b", vec![1, 2]),
        ];
        let e = MixedRadixEnumerator::new(&axes);
        assert_eq!(e.trial_count(), 34);
        assert_eq!(e.digit_bits(), 8);
        let offsets = e.offsets(4);
        assert_eq!(offsets, vec![2, 0]);
        assert_eq!(e.trial_code(4), "0x0200");
    }

    // ---------- round-trip invariants ----------

    #[test]
    fn trial_index_inverse_of_offsets_roundtrips() {
        let axes = vec![
            axis("a", "x", "a", vec![1, 2, 3]),
            axis("b", "x", "b", vec![1, 2]),
        ];
        let e = MixedRadixEnumerator::new(&axes);
        for t in 0..e.trial_count() {
            let offsets = e.offsets(t);
            assert_eq!(e.trial_index(&offsets), t);
        }
    }

    #[test]
    fn each_offset_is_within_its_axis_cardinality() {
        let axes = vec![
            axis("a", "x", "a", vec![1, 2, 3, 4]),
            axis("b", "x", "b", vec![1, 2]),
            axis("c", "x", "c", vec![1, 2, 3]),
        ];
        let e = MixedRadixEnumerator::new(&axes);
        for t in 0..e.trial_count() {
            let offs = e.offsets(t);
            for (i, o) in offs.iter().enumerate() {
                assert!(*o < e.cardinalities()[i]);
            }
        }
    }
}
