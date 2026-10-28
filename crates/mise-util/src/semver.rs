use nodejs_semver::{Range, Version as NodeVersion};
use std::cmp::Ordering;
use versions::{MChunk, Mess, Sep, Versioning};

/// splits a version number into an optional prefix and the remaining version string
pub fn split_version_prefix(version: &str) -> (String, String) {
    let mut prev_char = None;
    for (i, c) in version.char_indices() {
        if c.is_ascii_digit()
            && (i == 0 || prev_char.is_some_and(|c| ['-', '_', '/', '.', 'v', 'V'].contains(&c)))
        {
            let (prefix, version) = version.split_at(i);
            return (prefix.into(), version.into());
        }
        prev_char = Some(c);
    }
    ("".into(), version.into())
}

/// split a version number into chunks
/// given v: "1.2-3a4" return ["1", ".2", "-3a4"]
pub fn chunkify_version(v: &str) -> Vec<String> {
    fn chunkify(m: &Mess, sep0: &str, chunks: &mut Vec<String>) {
        for (i, chunk) in m.chunks.iter().enumerate() {
            let sep = if i == 0 { sep0 } else { "." };
            chunks.push(format!("{sep}{chunk}"));
        }
        if let Some((next_sep, next_mess)) = &m.next {
            chunkify(next_mess, next_sep.to_string().as_ref(), chunks)
        }
    }

    /// Hang SemVer build metadata (`+k3s1`) off the end of a `Mess`.
    ///
    /// Before versions 8.0.1, `SemVer::to_mess` only emitted the metadata when
    /// the version also had a pre-release, and `Version::to_mess` dropped it
    /// outright, so `1.36.4+k3s1` came back as a bare `1.36.4`. That made
    /// `--bump` rewrite a k3s pin to `1.37.0`, a tag that does not exist — the
    /// release is `v1.37.0+k3s1`. Newer releases emit it themselves, so only
    /// attach it when the `Mess` does not already end with it.
    fn attach_meta(m: &mut Mess, meta: &str) {
        // An epoch or a release already occupies `next`; the metadata goes last.
        match &mut m.next {
            Some((Sep::Plus, next)) if next.next.is_none() && next.to_string() == meta => {}
            Some((_, next)) => attach_meta(next, meta),
            None => {
                m.next = Some((
                    Sep::Plus,
                    Box::new(Mess {
                        chunks: vec![MChunk::Plain(meta.to_string())],
                        next: None,
                    }),
                ))
            }
        }
    }

    let mut chunks = vec![];
    // don't parse "latest", otherwise bump from latest to any version would have one chunk only
    if v != "latest"
        && let Some(v) = Versioning::new(v)
    {
        let m = match v {
            Versioning::Ideal(sem_ver) => {
                let mut m = sem_ver.to_mess();
                // With a pre-release, `to_mess` already carried the metadata over.
                if let Some(meta) = &sem_ver.meta
                    && sem_ver.pre_rel.is_none()
                {
                    attach_meta(&mut m, meta);
                }
                m
            }
            Versioning::General(version) => {
                let mut m = version.to_mess();
                if let Some(meta) = &version.meta {
                    attach_meta(&mut m, meta);
                }
                m
            }
            Versioning::Complex(mess) => mess,
        };
        chunkify(&m, "", &mut chunks);
    }
    chunks
}

/// Filter a list of version strings with an npm-compatible semver range.
///
/// Returns `None` for non-range queries so callers can fall back to mise's
/// existing fuzzy matching for aliases and non-semver tools.
pub fn npm_semver_range_filter(versions: &[String], query: &str) -> Option<Vec<String>> {
    let query = query.trim();
    if !is_npm_semver_range_query(query) {
        return None;
    }
    let range = Range::parse(query).ok()?;

    Some(
        versions
            .iter()
            .filter(|v| {
                let version = v.as_str();
                NodeVersion::parse(version)
                    .or_else(|_| NodeVersion::parse(version.trim_start_matches(['v', 'V'])))
                    .is_ok_and(|version| range.satisfies(&version))
            })
            .cloned()
            .collect(),
    )
}

pub fn is_npm_semver_range_query(query: &str) -> bool {
    if query.is_empty() || query.eq_ignore_ascii_case("latest") {
        return false;
    }
    // A real npm range never contains a control character, but `split_whitespace` below counts
    // tabs and newlines as separators, so a string that was mangled before it got here looks like
    // a multi-part range. TOML reading `\t` as a tab in `path = "C:\tools\node"` is how that
    // happens in practice. Calling such a value a semver range points the reader at versions when
    // the problem is the value itself; the validation that follows already names the real cause.
    if query.chars().any(|c| c.is_control()) {
        return false;
    }
    if query == "*" || query.eq_ignore_ascii_case("x") {
        return true;
    }
    if query.contains("||") || query.contains(" - ") {
        return true;
    }
    if matches!(
        query.as_bytes().first().copied(),
        Some(b'<' | b'>' | b'=' | b'^' | b'~')
    ) || query.contains('<')
        || query.contains('>')
    {
        return true;
    }
    if query.split_whitespace().count() > 1 {
        return true;
    }
    query.split('.').any(|part| matches!(part, "*" | "x" | "X"))
}

pub fn semver_triplet(version: &str) -> Option<(u64, u64, u64)> {
    let trimmed = version.trim().trim_start_matches(['v', 'V']);
    let mut parts = trimmed.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.split(['-', '+']).next()?.parse().ok()?;
    Some((major, minor, patch))
}

/// Compares complete SemVer versions, accepting one leading `v` or `V`.
/// Build metadata does not affect precedence. Returns `None` if either version
/// cannot be parsed, so callers can apply tool-specific comparison rules.
pub fn semver_precedence_cmp(current: &str, candidate: &str) -> Option<Ordering> {
    let parse = |v: &str| ::semver::Version::parse(v.strip_prefix(['v', 'V']).unwrap_or(v)).ok();
    Some(parse(current)?.cmp_precedence(&parse(candidate)?))
}

/// Compares numeric major, minor, and patch components, ignoring prerelease and
/// build suffixes. Returns `None` if either triplet cannot be extracted.
/// Use [`semver_precedence_cmp`] when prerelease ordering matters.
pub fn semver_cmp(version: &str, other: &str) -> Option<Ordering> {
    Some(semver_triplet(version)?.cmp(&semver_triplet(other)?))
}

pub fn semver_is_older_than(version: &str, minimum: &str) -> Option<bool> {
    Some(semver_cmp(version, minimum)? == Ordering::Less)
}

pub fn semver_is_at_least(version: &str, minimum: &str) -> Option<bool> {
    Some(semver_cmp(version, minimum)? != Ordering::Less)
}

/// One `.`-separated segment of the build metadata after a version's `+`.
///
/// Numbers order before text and compare as numbers, so build `101` follows
/// build `7` instead of preceding it as text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum BuildMetadataSegment {
    Number(u64),
    Text(String),
}

/// The build metadata of `v`, segment by segment; empty when there is none.
///
/// `Versioning` ignores everything after `+`, so this is the tie-break for
/// versions it calls equal, such as Java's `temurin-25.0.4+7.0.LTS` and
/// `temurin-25.0.4+101.0.LTS`.
pub fn build_metadata_segments(v: &str) -> Vec<BuildMetadataSegment> {
    let Some((_, meta)) = v.split_once('+') else {
        return vec![];
    };
    meta.split('.')
        .map(|seg| match seg.parse::<u64>() {
            Ok(n) => BuildMetadataSegment::Number(n),
            Err(_) => BuildMetadataSegment::Text(seg.to_string()),
        })
        .collect()
}

/// How `a`'s build metadata orders against `b`'s, where that can be told.
///
/// Decided by the first segments that differ, and only when both are
/// numbers: `+7.0.LTS` is `Less` than `+101.0.LTS`. Text that differs
/// (`+one` against `+two`), metadata on one side only, or no difference at
/// all is no evidence of a newer build, so that is `None`.
/// [`build_metadata_segments`] is the total order for sorting; this is the
/// strict subset an upgrade check can act on.
pub fn build_metadata_numeric_cmp(a: &str, b: &str) -> Option<Ordering> {
    let (a, b) = (build_metadata_segments(a), build_metadata_segments(b));
    let (x, y) = a.iter().zip(&b).find(|(x, y)| x != y)?;
    match (x, y) {
        (BuildMetadataSegment::Number(x), BuildMetadataSegment::Number(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BuildMetadataSegment, build_metadata_numeric_cmp, build_metadata_segments,
        chunkify_version, is_npm_semver_range_query, npm_semver_range_filter, semver_cmp,
        semver_is_at_least, semver_is_older_than, semver_precedence_cmp, semver_triplet,
        split_version_prefix,
    };
    use std::cmp::Ordering;

    #[test]
    fn test_build_metadata_segments() {
        assert_eq!(
            build_metadata_segments("25.0.4"),
            Vec::<BuildMetadataSegment>::new()
        );
        assert_eq!(
            build_metadata_segments("temurin-25.0.4+7.0.LTS"),
            vec![
                BuildMetadataSegment::Number(7),
                BuildMetadataSegment::Number(0),
                BuildMetadataSegment::Text("LTS".to_string()),
            ]
        );
        // numbers compare as numbers, and before text
        assert!(build_metadata_segments("1+7") < build_metadata_segments("1+101"));
        assert!(build_metadata_segments("1+7") < build_metadata_segments("1+k3s1"));
    }

    /// Only a numeric difference says which build is newer; everything else
    /// is left to the caller's "same version" default.
    #[test]
    fn test_build_metadata_numeric_cmp() {
        assert_eq!(
            build_metadata_numeric_cmp("temurin-25.0.4+7.0.LTS", "temurin-25.0.4+101.0.LTS"),
            Some(Ordering::Less)
        );
        assert_eq!(
            build_metadata_numeric_cmp("25.0.4+101.0.LTS", "25.0.4+7.0.LTS"),
            Some(Ordering::Greater)
        );
        assert_eq!(
            build_metadata_numeric_cmp("25.0.4+7.0.LTS", "25.0.4+7.1.LTS"),
            Some(Ordering::Less)
        );
        assert_eq!(build_metadata_numeric_cmp("1.2.3+one", "1.2.3+two"), None);
        assert_eq!(build_metadata_numeric_cmp("1.2.3+7", "1.2.3"), None);
        assert_eq!(build_metadata_numeric_cmp("1.2.3", "1.2.3+7"), None);
        assert_eq!(build_metadata_numeric_cmp("1.2.3+7", "1.2.3+7.1"), None);
        assert_eq!(build_metadata_numeric_cmp("1.2.3+7", "1.2.3+7"), None);
    }

    #[test]
    fn test_split_version_prefix() {
        assert_eq!(split_version_prefix("latest"), ("".into(), "latest".into()));
        assert_eq!(split_version_prefix("v1.2.3"), ("v".into(), "1.2.3".into()));
        assert_eq!(
            split_version_prefix("mountpoint-s3-v1.2.3-5_beta.5"),
            ("mountpoint-s3-v".into(), "1.2.3-5_beta.5".into())
        );
        assert_eq!(
            split_version_prefix("cli/1.2.3"),
            ("cli/".into(), "1.2.3".into())
        );
        assert_eq!(
            split_version_prefix("temurin-17.0.7+7"),
            ("temurin-".into(), "17.0.7+7".into())
        );
        assert_eq!(
            split_version_prefix("\u{5de5}\u{5177}-v1.2.3"),
            ("\u{5de5}\u{5177}-v".into(), "1.2.3".into())
        );
        assert_eq!(split_version_prefix("1.2"), ("".into(), "1.2".into()));
        assert_eq!(
            split_version_prefix("2:1.2.1"),
            ("".into(), "2:1.2.1".into())
        );
        assert_eq!(
            split_version_prefix("2025-05-17"),
            ("".into(), "2025-05-17".into())
        );
    }

    #[test]
    fn test_chunkify_version() {
        assert_eq!(chunkify_version("1.2-3a4"), vec!["1", ".2", "-3a4"]);
        assert_eq!(chunkify_version("latest"), Vec::<String>::new());
        assert_eq!(chunkify_version("1.0.0"), vec!["1", ".0", ".0"]);
        assert_eq!(
            chunkify_version("2.3.4-beta"),
            vec!["2", ".3", ".4", "-beta"]
        );
    }

    #[test]
    fn test_chunkify_version_keeps_build_metadata() {
        // k3s tags its releases `v1.36.4+k3s1`. Dropping `+k3s1` here made
        // `mise upgrade --bump` pin `1.37.0`, which is not a tag that exists.
        assert_eq!(
            chunkify_version("1.36.4+k3s1"),
            vec!["1", ".36", ".4", "+k3s1"]
        );
        // Java's `17.0.7+7` lost its build number the same way.
        assert_eq!(chunkify_version("17.0.7+7"), vec!["17", ".0", ".7", "+7"]);
        // A pre-release already kept its metadata; it must not be duplicated.
        assert_eq!(
            chunkify_version("1.2.3-rc1+meta"),
            vec!["1", ".2", ".3", "-rc1", "+meta"]
        );
        // Non-semver versions carry metadata too, epoch and release included.
        assert_eq!(
            chunkify_version("1.2.3a+meta"),
            vec!["1", ".2", ".3a", "+meta"]
        );
        assert_eq!(
            chunkify_version("2:1.2.3-r1+meta"),
            vec!["2", ":1", ".2", ".3", "-r1", "+meta"]
        );
    }

    #[test]
    fn test_npm_semver_range_filter_lower_bound() {
        let versions = ["25.5.0", "25.6.1", "25.8.2"].map(String::from).to_vec();

        assert_eq!(
            npm_semver_range_filter(&versions, ">=25.6.1").unwrap(),
            vec!["25.6.1".to_string(), "25.8.2".to_string()]
        );
        assert_eq!(
            npm_semver_range_filter(&versions, ">= 25.6.1").unwrap(),
            vec!["25.6.1".to_string(), "25.8.2".to_string()]
        );
    }

    #[test]
    fn test_npm_semver_range_filter_compound_bounds() {
        let versions = ["25.5.0", "25.6.1", "25.8.2", "26.0.0"]
            .map(String::from)
            .to_vec();

        assert_eq!(
            npm_semver_range_filter(&versions, ">=25.6.1 <26").unwrap(),
            vec!["25.6.1".to_string(), "25.8.2".to_string()]
        );
    }

    #[test]
    fn test_npm_semver_range_filter_caret() {
        let versions = ["20.0.0", "20.0.1", "20.1.0", "21.0.0"]
            .map(String::from)
            .to_vec();

        assert_eq!(
            npm_semver_range_filter(&versions, "^20.0.1").unwrap(),
            vec!["20.0.1".to_string(), "20.1.0".to_string()]
        );
    }

    #[test]
    fn test_npm_semver_range_filter_alternatives() {
        let versions = ["18.19.0", "20.0.0", "21.9.0", "22.0.0"]
            .map(String::from)
            .to_vec();

        assert_eq!(
            npm_semver_range_filter(&versions, ">=18 <20 || >=22").unwrap(),
            vec!["18.19.0".to_string(), "22.0.0".to_string()]
        );
    }

    #[test]
    fn test_npm_semver_range_filter_preserves_v_prefix() {
        let versions = ["v25.6.1", "v25.8.2"].map(String::from).to_vec();

        assert_eq!(
            npm_semver_range_filter(&versions, ">=25.8.0").unwrap(),
            vec!["v25.8.2".to_string()]
        );
    }

    #[test]
    fn test_npm_semver_range_filter_non_range_queries_fall_back() {
        assert_eq!(
            npm_semver_range_filter(&["1.0.0".to_string()], "latest"),
            None
        );
        assert_eq!(
            npm_semver_range_filter(&["1.0.0".to_string()], "temurin-"),
            None
        );
        assert_eq!(
            npm_semver_range_filter(&["1.0.0".to_string()], "1.0.0"),
            None
        );
    }

    #[test]
    fn test_npm_semver_range_query_ignores_mangled_values() {
        // A value broken before it reached here is not a range. TOML reads `\t` in
        // `path = "C:\tools\node"` as a tab and `\n` as a newline, which `split_whitespace`
        // counts as separators -- so it used to be reported as a multi-part semver range, with
        // no mention of the path that was actually wrong.
        assert!(!is_npm_semver_range_query("path:C:\tools\node"));
        assert!(!is_npm_semver_range_query("1.0\t2.0"));
        assert!(!is_npm_semver_range_query("1.0\n2.0"));
    }

    #[test]
    fn test_npm_semver_range_query_still_detects_real_ranges() {
        // The control for the test above: skipping control characters must not stop any range
        // from being detected. None of these contain one.
        for q in [
            "^1.2.3",
            ">=1.0 <2.0",
            "1.x",
            "*",
            "1.2 - 1.5",
            "1.0 || 2.0",
        ] {
            assert!(is_npm_semver_range_query(q), "expected {q:?} to be a range");
        }
        // Ordinary values stay non-ranges, including a well-formed `path:`.
        for q in [
            "1.0.0",
            "latest",
            "path:C:/tools/node",
            "path:/opt/homebrew/opt/node@20",
        ] {
            assert!(
                !is_npm_semver_range_query(q),
                "expected {q:?} not to be a range"
            );
        }
    }

    #[test]
    fn test_semver_triplet() {
        assert_eq!(semver_triplet("1.2.3"), Some((1, 2, 3)));
        assert_eq!(semver_triplet("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(semver_triplet("V1.2.3"), Some((1, 2, 3)));
        assert_eq!(semver_triplet("1.2.3-pre.1"), Some((1, 2, 3)));
        assert_eq!(semver_triplet("1.2.3+build.1"), Some((1, 2, 3)));
        assert_eq!(semver_triplet("1.2"), None);
        assert_eq!(semver_triplet("latest"), None);
        assert_eq!(semver_triplet("garbage"), None);
    }

    /// Protects the boundary between strict SemVer precedence and the caller's
    /// handling of tool-specific versions that cannot be parsed as SemVer.
    #[test]
    fn test_semver_precedence_cmp() {
        assert_eq!(
            semver_precedence_cmp("v2.1.280", "2.1.278"),
            Some(Ordering::Greater)
        );
        assert_eq!(
            semver_precedence_cmp("1.2.3-rc.1", "V1.2.3"),
            Some(Ordering::Less)
        );
        assert_eq!(
            semver_precedence_cmp("v1.2.3+one", "1.2.3+two"),
            Some(Ordering::Equal)
        );
        for version in ["latest", "1.2", "3.7c", "vv1.2.3"] {
            assert_eq!(semver_precedence_cmp(version, "1.2.3"), None);
            assert_eq!(semver_precedence_cmp("1.2.3", version), None);
        }
    }

    #[test]
    fn test_semver_cmp() {
        assert_eq!(semver_cmp("1.2.9", "1.3.0"), Some(Ordering::Less));
        assert_eq!(semver_cmp("1.3.0", "1.3.0"), Some(Ordering::Equal));
        assert_eq!(semver_cmp("1.3.1", "1.3.0"), Some(Ordering::Greater));
        assert_eq!(semver_cmp("latest", "1.3.0"), None);
    }

    #[test]
    fn test_semver_minimum_helpers() {
        assert_eq!(semver_is_older_than("1.2.9", "1.3.0"), Some(true));
        assert_eq!(semver_is_older_than("1.3.0", "1.3.0"), Some(false));
        assert_eq!(semver_is_older_than("v1.3.1", "1.3.0"), Some(false));
        assert_eq!(semver_is_older_than("latest", "1.3.0"), None);

        assert_eq!(semver_is_at_least("1.2.9", "1.3.0"), Some(false));
        assert_eq!(semver_is_at_least("1.3.0", "1.3.0"), Some(true));
        assert_eq!(semver_is_at_least("v1.3.1", "1.3.0"), Some(true));
        assert_eq!(semver_is_at_least("latest", "1.3.0"), None);
    }
}
