/// Output-space damage rectangle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rect {
    /// Horizontal offset from the output's left edge.
    pub x: u32,
    /// Vertical offset from the output's top edge.
    pub y: u32,
    /// Rectangle width in pixels.
    pub width: u32,
    /// Rectangle height in pixels.
    pub height: u32,
}

impl Rect {
    /// Creates a rectangle from its origin and dimensions.
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Creates a rectangle covering an output of the given dimensions.
    pub fn full(width: u32, height: u32) -> Self {
        Self::new(0, 0, width, height)
    }

    /// Returns the rectangle's area in pixels.
    pub fn area(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// Clips the rectangle to an output, returning `None` if it is empty.
    pub fn clip(self, width: u32, height: u32) -> Option<Self> {
        let x2 = self.x.saturating_add(self.width).min(width);
        let y2 = self.y.saturating_add(self.height).min(height);
        let x = self.x.min(width);
        let y = self.y.min(height);
        (x2 > x && y2 > y).then(|| Self::new(x, y, x2 - x, y2 - y))
    }

    /// Returns the smallest rectangle containing both inputs.
    pub fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let x2 = self
            .x
            .saturating_add(self.width)
            .max(other.x.saturating_add(other.width));
        let y2 = self
            .y
            .saturating_add(self.height)
            .max(other.y.saturating_add(other.height));
        Self::new(x, y, x2 - x, y2 - y)
    }
}

/// Controls how damage rectangles are simplified before presentation.
///
/// Damage tracking trades the cost of presenting additional rectangles against
/// the cost of transmitting pixels that did not change. This policy controls
/// three simplifications, applied in order:
///
/// 1. Touching rectangles are merged when their bounding union is no more than
///    `merge_numerator / merge_denominator` times their combined area. Raising
///    this ratio favors fewer rectangles at the cost of retransmitting more
///    unchanged pixels.
/// 2. Damage is promoted to the entire output when its total area reaches
///    `full_numerator / full_denominator` of the output area. Lowering this
///    ratio causes full-frame updates sooner.
/// 3. If more than `rectangle_limit` rectangles remain, they may be replaced by
///    their bounding rectangle when its area is no more than
///    `bounds_numerator / bounds_denominator` times the damaged area. Raising
///    this ratio makes bounding-box coalescing more aggressive.
///
/// The defaults merge touching rectangles with at most 25% area overhead,
/// switch to a full frame once at least half the output is damaged, and, above
/// 32 rectangles, use one bounding rectangle with at most 100% area overhead.
///
/// Applications for which each rectangle has high presentation overhead should
/// use larger merge and bounds ratios or a lower rectangle limit. Applications
/// for which pixel transfer is more expensive should use smaller ratios and a
/// higher rectangle limit. All denominators must be nonzero.
#[derive(Clone, Copy, Debug)]
pub struct DamagePolicy {
    /// Numerator of the allowed union-to-input-area ratio for pairwise merging.
    pub merge_numerator: u64,
    /// Denominator of the allowed union-to-input-area ratio; must be nonzero.
    pub merge_denominator: u64,
    /// Numerator of the damaged-to-output-area threshold for full-frame promotion.
    pub full_numerator: u64,
    /// Denominator of the full-frame promotion threshold; must be nonzero.
    pub full_denominator: u64,
    /// Rectangle count above which bounding-box coalescing is considered.
    pub rectangle_limit: usize,
    /// Numerator of the allowed bounds-to-damaged-area ratio for coalescing.
    pub bounds_numerator: u64,
    /// Denominator of the bounds-to-damaged-area ratio; must be nonzero.
    pub bounds_denominator: u64,
}

impl Default for DamagePolicy {
    fn default() -> Self {
        Self {
            merge_numerator: 5,
            merge_denominator: 4,
            full_numerator: 1,
            full_denominator: 2,
            rectangle_limit: 32,
            bounds_numerator: 2,
            bounds_denominator: 1,
        }
    }
}

/// Clips and coalesces damage rectangles for an output.
///
/// Returns one full-output rectangle when `force_full` is true or when damage
/// reaches the policy's full-frame threshold. Zero-sized outputs return no
/// rectangles.
pub fn plan_damage(
    width: u32,
    height: u32,
    rects: impl IntoIterator<Item = Rect>,
    force_full: bool,
    policy: DamagePolicy,
) -> Vec<Rect> {
    let full = Rect::full(width, height);
    if width == 0 || height == 0 {
        return Vec::new();
    }
    if force_full {
        return vec![full];
    }
    let mut rects: Vec<_> = rects
        .into_iter()
        .filter_map(|rect| rect.clip(width, height))
        .collect();

    let mut changed = true;
    while changed {
        changed = false;
        'outer: for left in 0..rects.len() {
            for right in left + 1..rects.len() {
                if merge_efficiently(rects[left], rects[right], policy) {
                    rects[left] = rects[left].union(rects[right]);
                    rects.swap_remove(right);
                    changed = true;
                    break 'outer;
                }
            }
        }
    }

    let damaged: u64 = rects.iter().map(|rect| rect.area()).sum();
    let screen = full.area();
    if damaged.saturating_mul(policy.full_denominator)
        >= screen.saturating_mul(policy.full_numerator)
    {
        return vec![full];
    }
    if rects.len() > policy.rectangle_limit {
        let bounds = rects.iter().copied().reduce(Rect::union).unwrap_or(full);
        if bounds.area().saturating_mul(policy.bounds_denominator)
            <= damaged.saturating_mul(policy.bounds_numerator)
        {
            return vec![bounds];
        }
    }
    rects
}

fn merge_efficiently(a: Rect, b: Rect, policy: DamagePolicy) -> bool {
    let touches = a.x <= b.x.saturating_add(b.width)
        && b.x <= a.x.saturating_add(a.width)
        && a.y <= b.y.saturating_add(b.height)
        && b.y <= a.y.saturating_add(a.height);
    touches
        && a.union(b).area().saturating_mul(policy.merge_denominator)
            <= a.area()
                .saturating_add(b.area())
                .saturating_mul(policy.merge_numerator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clips_discards_and_merges_damage() {
        assert_eq!(
            plan_damage(
                100,
                100,
                [
                    Rect::new(1, 1, 4, 4),
                    Rect::new(5, 1, 4, 4),
                    Rect::new(200, 0, 2, 2)
                ],
                false,
                DamagePolicy::default(),
            ),
            [Rect::new(1, 1, 8, 4)]
        );
    }

    #[test]
    fn promotes_half_the_output_to_full() {
        assert_eq!(
            plan_damage(
                100,
                100,
                [Rect::new(0, 0, 50, 100)],
                false,
                DamagePolicy::default()
            ),
            [Rect::full(100, 100)]
        );
    }
}
