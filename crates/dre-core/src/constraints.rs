//! Plugin version constraints: intersecting semver requirements and detecting contradictions.

use semver::{Comparator, Op, Version, VersionReq};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Bound {
    v: Version,
    inclusive: bool,
}

/// `[lo, hi)`-style interval. `hi == None` is unbounded.
#[derive(Debug, Clone)]
struct Interval {
    lo: Bound,
    hi: Option<Bound>,
}

fn v(major: u64, minor: u64, patch: u64) -> Version {
    Version::new(major, minor, patch)
}

fn interval(c: &Comparator) -> Interval {
    let (maj, min, pat) = (c.major, c.minor, c.patch);
    let floor = v(maj, min.unwrap_or(0), pat.unwrap_or(0));
    // The next version after everything this partial version covers.
    let next = match (min, pat) {
        (None, _) => v(maj + 1, 0, 0),
        (Some(m), None) => v(maj, m + 1, 0),
        (Some(m), Some(p)) => v(maj, m, p + 1),
    };
    let inc = |v: Version| Bound { v, inclusive: true };
    let exc = |v: Version| Bound { v, inclusive: false };
    let zero = inc(v(0, 0, 0));
    match c.op {
        Op::Exact | Op::Wildcard => Interval {
            lo: inc(floor),
            hi: Some(exc(next)),
        },
        Op::Greater => Interval {
            lo: inc(next),
            hi: None,
        },
        Op::GreaterEq => Interval {
            lo: inc(floor),
            hi: None,
        },
        Op::Less => Interval {
            lo: zero,
            hi: Some(exc(floor)),
        },
        Op::LessEq => Interval {
            lo: zero,
            hi: Some(exc(next)),
        },
        Op::Tilde => {
            let hi = match min {
                None => v(maj + 1, 0, 0),
                Some(m) => v(maj, m + 1, 0),
            };
            Interval {
                lo: inc(floor),
                hi: Some(exc(hi)),
            }
        }
        Op::Caret => {
            let hi = match (maj, min, pat) {
                (0, None, _) => v(1, 0, 0),
                (0, Some(0), None) => v(0, 1, 0),
                (0, Some(0), Some(p)) => v(0, 0, p + 1),
                (0, Some(m), _) => v(0, m + 1, 0),
                (x, _, _) => v(x + 1, 0, 0),
            };
            Interval {
                lo: inc(floor),
                hi: Some(exc(hi)),
            }
        }
        _ => Interval { lo: zero, hi: None },
    }
}

fn intersect(a: Interval, b: Interval) -> Interval {
    let lo = if (&a.lo.v, !a.lo.inclusive) >= (&b.lo.v, !b.lo.inclusive) {
        a.lo
    } else {
        b.lo
    };
    let hi = match (a.hi, b.hi) {
        (None, h) | (h, None) => h,
        (Some(x), Some(y)) => Some(if (&x.v, x.inclusive) <= (&y.v, y.inclusive) {
            x
        } else {
            y
        }),
    };
    Interval { lo, hi }
}

fn is_empty(i: &Interval) -> bool {
    match &i.hi {
        None => false,
        Some(hi) => i.lo.v > hi.v || (i.lo.v == hi.v && !(i.lo.inclusive && hi.inclusive)),
    }
}

/// Whether any version could satisfy every requirement at once.
pub fn compatible(reqs: &[&VersionReq]) -> bool {
    let all = Interval {
        lo: Bound {
            v: v(0, 0, 0),
            inclusive: true,
        },
        hi: None,
    };
    let i = reqs
        .iter()
        .flat_map(|r| r.comparators.iter())
        .map(interval)
        .fold(all, intersect);
    !is_empty(&i)
}

/// The combined requirement: every comparator from every requirement.
pub fn combine(reqs: &[&VersionReq]) -> VersionReq {
    let mut comparators: Vec<Comparator> = Vec::new();
    for c in reqs.iter().flat_map(|r| r.comparators.iter()) {
        if !comparators.contains(c) {
            comparators.push(c.clone());
        }
    }
    VersionReq { comparators }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(a: &str, b: &str) -> bool {
        compatible(&[&VersionReq::parse(a).unwrap(), &VersionReq::parse(b).unwrap()])
    }

    #[test]
    fn overlapping_ranges_are_compatible() {
        assert!(ok(">=1.0", "<2.0"));
        assert!(ok("^1.2", ">=1.4"));
        assert!(ok("*", "=0.3.1"));
        assert!(ok("<=1.2", ">=1.2.5"));
    }

    #[test]
    fn disjoint_ranges_are_contradictory() {
        assert!(!ok(">=2.0", "<1.0"));
        assert!(!ok("^1", "^2"));
        assert!(!ok("=1.2.3", ">1.2.3"));
        assert!(!ok("~0.3", ">=0.4"));
    }
}
