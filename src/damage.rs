/// Output-space damage rectangle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn full(width: u32, height: u32) -> Self {
        Self::new(0, 0, width, height)
    }

    pub fn area(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    pub fn clip(self, width: u32, height: u32) -> Option<Self> {
        let x2 = self.x.saturating_add(self.width).min(width);
        let y2 = self.y.saturating_add(self.height).min(height);
        let x = self.x.min(width);
        let y = self.y.min(height);
        (x2 > x && y2 > y).then(|| Self::new(x, y, x2 - x, y2 - y))
    }

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

#[derive(Clone, Copy, Debug)]
pub struct DamagePolicy {
    pub merge_numerator: u64,
    pub merge_denominator: u64,
    pub full_numerator: u64,
    pub full_denominator: u64,
    pub rectangle_limit: usize,
    pub bounds_numerator: u64,
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
