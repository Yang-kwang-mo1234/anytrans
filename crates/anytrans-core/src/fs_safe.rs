//! Turning paths received from another device into paths that are safe to
//! create on this one, whatever operating system either side runs.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Converts a `/`-separated relative path from the wire into a local relative path.
///
/// Paths that try to leave the destination (`..`, absolute paths) are rejected.
/// Names Windows cannot store are rewritten, on every platform, so a transfer
/// lands under the same names wherever it is received.
pub fn sanitize_rel_path(rel: &str) -> Result<PathBuf> {
    if rel.is_empty() || rel.contains('\0') {
        bail!("invalid path {rel:?}");
    }
    let mut out = PathBuf::new();
    for component in rel.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            bail!("unsafe path {rel:?}");
        }
        out.push(sanitize_component(component));
    }
    Ok(out)
}

fn sanitize_component(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\' | '/' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    // Windows silently drops trailing dots and spaces.
    s.truncate(s.trim_end_matches(['.', ' ']).len());
    if s.is_empty() {
        return "_".to_string();
    }
    let stem = s.split('.').next().unwrap_or_default();
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        s.insert(0, '_');
    }
    s
}

/// Returns `dir/name`, or `dir/name (1)`, `dir/name (2)`, ... if that is taken.
/// For files the counter goes before the extension.
pub fn unique_dest(dir: &Path, name: &str, is_dir: bool) -> PathBuf {
    let first = dir.join(name);
    if first.symlink_metadata().is_err() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if !is_dir && dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    (1u32..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| p.symlink_metadata().is_err())
        .expect("an unused name exists")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_paths_that_escape() {
        for bad in [
            "",
            "..",
            "../x",
            "a/../b",
            "/etc/passwd",
            "a//b",
            "a/./b",
            "a\0b",
        ] {
            assert!(
                sanitize_rel_path(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn keeps_ordinary_paths() {
        assert_eq!(
            sanitize_rel_path("사진/2026/a b.jpg").unwrap(),
            Path::new("사진").join("2026").join("a b.jpg")
        );
    }

    #[test]
    fn rewrites_names_windows_cannot_store() {
        let cases = [
            ("CON", "_CON"),
            ("nul.txt", "_nul.txt"),
            ("a:b?.txt", "a_b_.txt"),
            ("..\\evil", ".._evil"),
            ("C:\\x", "C__x"),
            ("trailing. ", "trailing"),
            ("...", "_"),
            ("console.log", "console.log"),
        ];
        for (input, want) in cases {
            assert_eq!(
                sanitize_rel_path(input).unwrap(),
                Path::new(want),
                "{input:?}"
            );
        }
    }

    #[test]
    fn unique_dest_counts_up() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        assert_eq!(unique_dest(d, "a.txt", false), d.join("a.txt"));
        std::fs::write(d.join("a.txt"), b"").unwrap();
        assert_eq!(unique_dest(d, "a.txt", false), d.join("a (1).txt"));
        std::fs::write(d.join("a (1).txt"), b"").unwrap();
        assert_eq!(unique_dest(d, "a.txt", false), d.join("a (2).txt"));
        std::fs::create_dir(d.join("my.dir")).unwrap();
        assert_eq!(unique_dest(d, "my.dir", true), d.join("my.dir (1)"));
    }
}
