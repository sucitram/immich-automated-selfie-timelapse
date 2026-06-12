//! Shared utility functions.

use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};

/// Build a unique job slug that encodes person, date range, and album selection.
///
/// Format: `{person}_{date_segment}[_{album_segment}]`
///
/// This is the primary output folder name when running a job. Including dates
/// and albums in the slug means multiple jobs for the same person coexist on
/// disk without overwriting each other.
///
/// Note: The frontend has a matching `makeJobSlug` in `frontend/src/lib/utils.js`
/// that must be kept in sync with this function.
pub fn make_job_slug(
    person_name: Option<&str>,
    person_id: &str,
    date_from: Option<&str>,
    date_to: Option<&str>,
    album_names: &[String],
) -> String {
    let person: String = sanitize_folder_name(person_name, person_id)
        .chars()
        .take(30)
        .collect();

    let dates = match (
        date_from.filter(|s| !s.is_empty()),
        date_to.filter(|s| !s.is_empty()),
    ) {
        (None, None) => "all".to_string(),
        (Some(f), None) => format!("from_{}", sanitize_date_str(f)),
        (None, Some(t)) => format!("to_{}", sanitize_date_str(t)),
        (Some(f), Some(t)) => format!("{}_{}", sanitize_date_str(f), sanitize_date_str(t)),
    };

    let slug = match build_album_slug(album_names) {
        Some(album_part) => format!("{}_{}_{}", person, dates, album_part),
        None => format!("{}_{}", person, dates),
    };

    slug.chars().take(80).collect()
}

/// Keep only filesystem-safe characters from a date string and cap at 10 chars (YYYY-MM-DD).
fn sanitize_date_str(date: &str) -> String {
    date.chars()
        .take(10)
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect()
}

/// Build a short, human-readable album suffix from album names.
fn build_album_slug(album_names: &[String]) -> Option<String> {
    match album_names.len() {
        0 => None,
        1 => {
            let s: String = sanitize_folder_name(Some(&album_names[0]), "album")
                .chars()
                .take(20)
                .collect();
            Some(s)
        }
        2 => {
            let a: String = sanitize_folder_name(Some(&album_names[0]), "album")
                .chars()
                .take(10)
                .collect();
            let b: String = sanitize_folder_name(Some(&album_names[1]), "album")
                .chars()
                .take(10)
                .collect();
            let combined = format!("{}_{}", a, b);
            if combined.len() <= 25 {
                Some(combined)
            } else {
                Some(format!("{}_albums", album_names.len()))
            }
        }
        n => Some(format!("{}_albums", n)),
    }
}

/// Create a safe snake_case folder name from person name or ID.
///
/// This sanitizes user input to create safe filesystem paths by:
/// - Using the person name if provided and non-empty, otherwise falling back to ID
/// - Removing accents and diacritical marks
/// - Converting to lowercase
/// - Replacing spaces with underscores
/// - Replacing unsafe characters with underscores
/// - Trimming whitespace and underscores
/// - Limiting length to 50 characters
///
/// Note: The frontend has a JavaScript version of this function that must be
/// kept in sync. See `frontend/src/lib/components/App.svelte`.
pub fn sanitize_folder_name(name: Option<&str>, id: &str) -> String {
    let base = name.filter(|n| !n.is_empty()).unwrap_or(id);

    // Remove accents by decomposing to NFD and filtering combining marks
    let without_accents: String = base.nfd().filter(|c| !is_combining_mark(*c)).collect();

    // Convert to lowercase and replace unsafe characters with underscores
    let sanitized: String = without_accents
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();

    // Trim whitespace/underscores and limit length (char-based to avoid UTF-8 boundary panics)
    let trimmed = sanitized.trim_matches(|c: char| c.is_whitespace() || c == '_');
    trimmed.chars().take(50).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_folder_name_with_name() {
        assert_eq!(sanitize_folder_name(Some("John Doe"), "abc123"), "john_doe");
    }

    #[test]
    fn test_sanitize_folder_name_falls_back_to_id() {
        assert_eq!(sanitize_folder_name(None, "abc123"), "abc123");
        assert_eq!(sanitize_folder_name(Some(""), "abc123"), "abc123");
    }

    #[test]
    fn test_sanitize_folder_name_replaces_unsafe_chars() {
        assert_eq!(
            sanitize_folder_name(Some("John/Doe:Test"), "id"),
            "john_doe_test"
        );
    }

    #[test]
    fn test_sanitize_folder_name_trims_whitespace() {
        assert_eq!(sanitize_folder_name(Some("  John Doe  "), "id"), "john_doe");
    }

    #[test]
    fn test_sanitize_folder_name_limits_length() {
        let long_name = "A".repeat(100);
        let result = sanitize_folder_name(Some(&long_name), "id");
        assert_eq!(result.chars().count(), 50);
    }

    #[test]
    fn test_sanitize_folder_name_limits_length_multibyte() {
        // CJK characters are multi-byte in UTF-8 but pass is_alphanumeric()
        let long_name = "\u{4e2d}".repeat(100); // 100 CJK characters
        let result = sanitize_folder_name(Some(&long_name), "id");
        assert_eq!(result.chars().count(), 50);
    }

    #[test]
    fn test_make_job_slug_no_dates_no_albums() {
        assert_eq!(
            make_job_slug(Some("Alice"), "id", None, None, &[]),
            "alice_all"
        );
    }

    #[test]
    fn test_make_job_slug_date_range() {
        assert_eq!(
            make_job_slug(Some("Alice"), "id", Some("2020-01-01"), Some("2024-12-31"), &[]),
            "alice_2020-01-01_2024-12-31"
        );
    }

    #[test]
    fn test_make_job_slug_date_from_only() {
        assert_eq!(
            make_job_slug(Some("Alice"), "id", Some("2020-01-01"), None, &[]),
            "alice_from_2020-01-01"
        );
    }

    #[test]
    fn test_make_job_slug_single_album() {
        assert_eq!(
            make_job_slug(Some("Alice"), "id", None, None, &["Holidays".to_string()]),
            "alice_all_holidays"
        );
    }

    #[test]
    fn test_make_job_slug_two_albums() {
        assert_eq!(
            make_job_slug(
                Some("Alice"),
                "id",
                None,
                None,
                &["Holidays".to_string(), "Travel".to_string()]
            ),
            "alice_all_holidays_travel"
        );
    }

    #[test]
    fn test_make_job_slug_many_albums() {
        let albums: Vec<String> = (1..=5).map(|i| format!("Album {}", i)).collect();
        assert_eq!(
            make_job_slug(Some("Alice"), "id", None, None, &albums),
            "alice_all_5_albums"
        );
    }

    #[test]
    fn test_make_job_slug_date_and_album() {
        assert_eq!(
            make_job_slug(
                Some("Alice"),
                "id",
                Some("2024-01-01"),
                Some("2024-12-31"),
                &["Christmas".to_string()]
            ),
            "alice_2024-01-01_2024-12-31_christmas"
        );
    }

    #[test]
    fn test_sanitize_folder_name_removes_accents() {
        // French accents
        assert_eq!(sanitize_folder_name(Some("José"), "id"), "jose");
        assert_eq!(sanitize_folder_name(Some("René"), "id"), "rene");
        assert_eq!(sanitize_folder_name(Some("François"), "id"), "francois");

        // German umlauts
        assert_eq!(sanitize_folder_name(Some("Müller"), "id"), "muller");
        assert_eq!(sanitize_folder_name(Some("Björk"), "id"), "bjork");

        // Spanish
        assert_eq!(sanitize_folder_name(Some("Peña"), "id"), "pena");

        // Mixed accents
        assert_eq!(
            sanitize_folder_name(Some("Élève Français"), "id"),
            "eleve_francais"
        );
    }
}
