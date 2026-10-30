//! Version predicates used by mod metadata.
//!
//! Only the dialects that appear in real mod files are supported: Fabric/Quilt
//! comparators (`>=1.20.1 <1.21`, `~1.20.1`, `1.20.x`, `||`) and Maven ranges
//! (`[1.20.1,1.21)`). Unparseable predicates return `None`: callers skip the
//! check instead of guessing, so a weird version never turns into a false
//! "incompatible" warning.

use std::cmp::Ordering;

/// Does `version` satisfy `constraint`? `None` means the predicate is not
/// understood and the caller must stay quiet.
pub fn matches(version: &str, constraint: &str) -> Option<bool> {
    let constraint = constraint.trim();
    if constraint.is_empty() || constraint == "*" || constraint == "x" {
        return Some(true);
    }
    if constraint.starts_with('[') || constraint.starts_with('(') {
        return maven_range(version, constraint);
    }
    let mut understood = false;
    for alternative in constraint.split("||") {
        let mut all = true;
        let mut tokens = 0;
        for token in alternative.split_whitespace() {
            tokens += 1;
            match token_matches(version, token) {
                Some(true) => {}
                Some(false) => all = false,
                None => return None,
            }
        }
        if tokens == 0 {
            continue;
        }
        understood = true;
        if all {
            return Some(true);
        }
    }
    understood.then_some(false)
}

/// True when at least one predicate in `ranges` matches. Empty means any.
pub fn any_matches(version: &str, ranges: &[String]) -> Option<bool> {
    if ranges.is_empty() {
        return Some(true);
    }
    let mut understood = false;
    for range in ranges {
        match matches(version, range) {
            Some(true) => return Some(true),
            Some(false) => understood = true,
            None => {}
        }
    }
    understood.then_some(false)
}

/// Forge-family metadata writes the Minecraft requirement as a Maven range and
/// is routinely sloppy: `[1.21,1.21.1)` means "the 1.21 series", and a bare
/// version is a recommendation rather than a hard requirement. Patch releases
/// keep the same modding API, so compare `major.minor` and only report a
/// mismatch when the series itself is outside the declared range.
pub fn any_matches_series(version: &str, ranges: &[String]) -> Option<bool> {
    if ranges.is_empty() {
        return Some(true);
    }
    let series = series_of(version)?;
    let mut understood = false;
    for range in ranges {
        let range = range.trim();
        if range.is_empty() || range == "*" || range == "x" {
            return Some(true);
        }
        if !range.starts_with('[') && !range.starts_with('(') {
            // A bare version is a recommendation for Forge-style metadata.
            return Some(true);
        }
        understood = true;
        if series_in(series, range) {
            return Some(true);
        }
    }
    understood.then_some(false)
}

fn series_of(text: &str) -> Option<(u64, u64)> {
    let text = text.trim().trim_start_matches('v');
    let mut parts = text.split(['.', '-', '+', '_']);
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    Some((major, minor))
}

/// Patch-level bounds are compared by series, but a range that spans series
/// (`[1.20.1,1.21)`) still has to exclude them.
fn series_in(series: (u64, u64), range: &str) -> bool {
    let Some(body) = range.get(1..range.len().saturating_sub(1)) else {
        return true;
    };
    if !body.contains(',') {
        return series_of(body).is_none_or(|bound| bound == series);
    }
    let lower_inclusive = range.starts_with('[');
    let upper_inclusive = range.ends_with(']');
    let (lower, upper) = body.split_once(',').unwrap_or(("", ""));
    let lower = series_of(lower);
    let upper = series_of(upper);
    if let (Some(low), Some(high)) = (lower, upper)
        && low == high
    {
        // `[1.21,1.21.1)`: one series with a patch-level upper bound.
        return series == low;
    }
    if let Some(low) = lower
        && (series < low || (series == low && !lower_inclusive))
    {
        return false;
    }
    if let Some(high) = upper
        && (series > high || (series == high && !upper_inclusive))
    {
        return false;
    }
    true
}

fn maven_range(version: &str, range: &str) -> Option<bool> {
    if range.len() < 2 {
        return None;
    }
    let bytes = range.as_bytes();
    let lower_inclusive = *bytes.first()? == b'[';
    let upper_inclusive = *bytes.last()? == b']';
    let body = &range[1..range.len() - 1];
    let version = Version::parse(version)?;
    if !body.contains(',') {
        // `[1.20.1]` pins one version.
        let bound = Version::parse(body)?;
        return Some(version.compare(&bound) == Ordering::Equal);
    }
    let (lower, upper) = body.split_once(',')?;
    if lower.trim().is_empty() && upper.trim().is_empty() {
        return Some(true);
    }
    if !lower.trim().is_empty() {
        let bound = Version::parse(lower)?;
        let ordering = version.compare(&bound);
        if ordering == Ordering::Less || (ordering == Ordering::Equal && !lower_inclusive) {
            return Some(false);
        }
    }
    if !upper.trim().is_empty() {
        let bound = Version::parse(upper)?;
        let ordering = version.compare(&bound);
        if ordering == Ordering::Greater || (ordering == Ordering::Equal && !upper_inclusive) {
            return Some(false);
        }
    }
    Some(true)
}

fn token_matches(version: &str, token: &str) -> Option<bool> {
    let token = token.trim();
    if token.is_empty() || token == "*" {
        return Some(true);
    }
    if let Some(rest) = token.strip_prefix(">=") {
        return Some(Version::parse(version)?.compare(&Version::parse(rest)?) != Ordering::Less);
    }
    if let Some(rest) = token.strip_prefix("<=") {
        return Some(Version::parse(version)?.compare(&Version::parse(rest)?) != Ordering::Greater);
    }
    if let Some(rest) = token.strip_prefix('>') {
        return Some(Version::parse(version)?.compare(&Version::parse(rest)?) == Ordering::Greater);
    }
    if let Some(rest) = token.strip_prefix('<') {
        return Some(Version::parse(version)?.compare(&Version::parse(rest)?) == Ordering::Less);
    }
    if let Some(rest) = token.strip_prefix('~') {
        let base = Version::parse(rest)?;
        let upper = base.tilde_upper()?;
        return Some(
            base.compare(&Version::parse(version)?) != Ordering::Greater
                && Version::parse(version)?.compare(&upper) == Ordering::Less,
        );
    }
    if let Some(rest) = token.strip_prefix('^') {
        let base = Version::parse(rest)?;
        let upper = base.caret_upper()?;
        return Some(
            base.compare(&Version::parse(version)?) != Ordering::Greater
                && Version::parse(version)?.compare(&upper) == Ordering::Less,
        );
    }
    let token = token.strip_prefix('=').unwrap_or(token);
    if has_wildcard(token) {
        return Some(wildcard_matches(version, token));
    }
    Some(Version::parse(version)?.compare(&Version::parse(token)?) == Ordering::Equal)
}

fn has_wildcard(token: &str) -> bool {
    token.eq_ignore_ascii_case("x")
        || token.contains('*')
        || token.split(['.', '-']).any(|part| part.eq_ignore_ascii_case("x"))
}

fn wildcard_matches(version: &str, token: &str) -> bool {
    let prefix = token
        .trim_end_matches('*')
        .split(['.', '-'])
        .take_while(|part| !part.eq_ignore_ascii_case("x") && !part.is_empty())
        .filter_map(|part| part.parse::<u64>().ok())
        .collect::<Vec<_>>();
    let actual = numeric_parts(version);
    if prefix.len() > actual.len() {
        return false;
    }
    prefix.iter().zip(actual.iter()).all(|(left, right)| left == right)
}

fn numeric_parts(version: &str) -> Vec<u64> {
    version
        .trim()
        .trim_start_matches('v')
        .split(['.', '-', '+', '_'])
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Version {
    parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Number(u64),
    Text(String),
}

impl Version {
    fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let text = match text.strip_prefix('v') {
            Some(rest) if rest.starts_with(|ch: char| ch.is_ascii_digit()) => rest,
            _ => text,
        };
        let mut parts = Vec::new();
        for chunk in text.split(['.', '-', '+', '_']) {
            if chunk.is_empty() {
                continue;
            }
            if let Ok(number) = chunk.parse::<u64>() {
                parts.push(Part::Number(number));
                continue;
            }
            // Suffixes glued to a number (`1.1.25a`, `2.3.4b`) are the same
            // release with a letter, not a pre-release of zero: split the
            // digits so `1.1.25a` still sorts above `1.1.20`.
            let digits: String = chunk.chars().take_while(char::is_ascii_digit).collect();
            match digits.parse::<u64>() {
                Ok(number) if !digits.is_empty() => {
                    parts.push(Part::Number(number));
                    let rest: String = chunk[digits.len()..].to_ascii_lowercase();
                    if !rest.is_empty() {
                        parts.push(Part::Text(rest));
                    }
                }
                _ => parts.push(Part::Text(chunk.to_ascii_lowercase())),
            }
        }
        // A predicate such as `banana` is not a version; reporting it as
        // incompatible would be a false positive, so keep it unknown.
        if !matches!(parts.first(), Some(Part::Number(_))) {
            return None;
        }
        Some(Self { parts })
    }

    fn compare(&self, other: &Self) -> Ordering {
        let len = self.parts.len().max(other.parts.len());
        for index in 0..len {
            match (self.parts.get(index), other.parts.get(index)) {
                (Some(Part::Number(left)), Some(Part::Number(right))) => match left.cmp(right) {
                    Ordering::Equal => {}
                    ordering => return ordering,
                },
                (Some(Part::Text(left)), Some(Part::Text(right))) => match left.cmp(right) {
                    Ordering::Equal => {}
                    ordering => return ordering,
                },
                // A release is newer than its pre-release (`1.20` > `1.20-pre1`).
                (Some(Part::Number(_)), Some(Part::Text(_))) => return Ordering::Greater,
                (Some(Part::Text(_)), Some(Part::Number(_))) => return Ordering::Less,
                (Some(Part::Number(number)), None) => {
                    if *number != 0 {
                        return Ordering::Greater;
                    }
                }
                (None, Some(Part::Number(number))) => {
                    if *number != 0 {
                        return Ordering::Less;
                    }
                }
                (Some(Part::Text(_)), None) => return Ordering::Less,
                (None, Some(Part::Text(_))) => return Ordering::Greater,
                (None, None) => {}
            }
        }
        Ordering::Equal
    }

    fn numeric(&self) -> Vec<u64> {
        self.parts
            .iter()
            .map_while(|part| match part {
                Part::Number(number) => Some(*number),
                Part::Text(_) => None,
            })
            .collect()
    }

    fn tilde_upper(&self) -> Option<Self> {
        let numbers = self.numeric();
        match numbers.len() {
            0 => None,
            1 => Some(Self {
                parts: vec![Part::Number(numbers[0] + 1)],
            }),
            _ => Some(Self {
                parts: vec![Part::Number(numbers[0]), Part::Number(numbers[1] + 1)],
            }),
        }
    }

    fn caret_upper(&self) -> Option<Self> {
        let numbers = self.numeric();
        let position = numbers.iter().position(|number| *number != 0)?;
        let mut upper = vec![Part::Number(0); position + 1];
        upper[position] = Part::Number(numbers[position] + 1);
        Some(Self { parts: upper })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yes(version: &str, range: &str) {
        assert_eq!(matches(version, range), Some(true), "{version} should match {range}");
    }

    fn no(version: &str, range: &str) {
        assert_eq!(
            matches(version, range),
            Some(false),
            "{version} should not match {range}"
        );
    }

    #[test]
    fn fabric_comparators() {
        yes("1.20.1", ">=1.20.1");
        no("1.20", ">=1.20.1");
        yes("1.20.4", ">=1.20.1 <1.21");
        no("1.21", ">=1.20.1 <1.21");
        yes("0.15.11", ">=0.15.0");
        yes("17.0.9", ">=17");
    }

    #[test]
    fn wildcards_and_tilde() {
        yes("1.20.1", "1.20.x");
        yes("1.20.11", "1.20.*");
        no("1.21", "1.20.x");
        yes("1.20.4", "~1.20.1");
        no("1.21", "~1.20.1");
        yes("1.20.9", "~1.20");
    }

    #[test]
    fn caret_follows_semver() {
        yes("1.9.0", "^1.2.3");
        no("2.0.0", "^1.2.3");
        no("0.3.0", "^0.2.3");
        yes("0.2.9", "^0.2.3");
        no("0.0.4", "^0.0.3");
    }

    #[test]
    fn maven_ranges() {
        yes("1.20.1", "[1.20.1,1.21)");
        no("1.21", "[1.20.1,1.21)");
        yes("1.21", "[1.20.1,1.21]");
        yes("47.2.0", "[47,)");
        no("46.0.0", "[47,)");
        no("1.20.1", "(,1.20]");
        yes("1.20", "(,1.20]");
        yes("1.20.1", "[1.20.1]");
        no("1.20.2", "[1.20.1]");
    }

    #[test]
    fn letter_suffixes_are_not_pre_releases() {
        // Real mod builds such as `1.1.25a` are `1.1.25` plus a letter; reading
        // the whole chunk as text made them sort below `1.1.20`.
        yes("1.1.25a", "[1.1.20,1.2.0)");
        yes("1.1.25a", ">=1.1.20");
        no("1.1.19a", ">=1.1.20");
        yes("2.0.0b", ">=1.9.9");
        no("1.1.25a", "[1.1.25,)");
        yes("1.1.25a", "[1.1.25a,)");
        // A dotted pre-release keeps sorting below the release itself.
        no("1.20-pre1", ">=1.20");
    }

    #[test]
    fn alternatives() {
        yes("1.20.1", "1.20.1 || 1.19.4");
        yes("1.19.4", "1.20.1 || 1.19.4");
        no("1.18.2", "1.20.1 || 1.19.4");
    }

    #[test]
    fn forge_game_ranges_are_series_lenient() {
        // Real NeoForge metadata: JEI declares `[1.21, 1.21.1)` for 1.21.1 and
        // a bare version is a recommendation, not a requirement.
        let yes_series = |version: &str, range: &str| {
            assert_eq!(
                any_matches_series(version, &[range.to_string()]),
                Some(true),
                "{version} should match {range}"
            );
        };
        let no_series = |version: &str, range: &str| {
            assert_eq!(
                any_matches_series(version, &[range.to_string()]),
                Some(false),
                "{version} should not match {range}"
            );
        };
        yes_series("1.21.1", "[1.21,1.21.1)");
        yes_series("1.21.1", "[1.21, 1.21.1)");
        yes_series("1.21.1", "[1.21,1.21z)");
        yes_series("1.21.1", "1.21");
        yes_series("1.21.1", "[1.21.1,1.21.2)");
        yes_series("1.21.1", "[1.20.5,)");
        no_series("1.21.1", "[1.20.1]");
        no_series("1.21.1", "[1.22,)");
        no_series("1.21.1", "[1.18,1.19]");
        no_series("1.21.1", "[1.20.1,1.21)");
    }

    #[test]
    fn unknown_predicates_stay_unknown() {
        assert_eq!(matches("1.20.1", "banana"), None);
        assert_eq!(matches("1.20.1", "[oops"), None);
    }
}
