//! Directory-tree renderer, ported from wcgw's `display_tree.DirectoryTree`.
//!
//! The repo context is shown as a partially-expanded tree: only the ranked
//! "interesting" files (and their parent directories) are expanded; everything
//! else in a directory collapses into a single `...` line so the LLM sees
//! structure without being flooded.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

pub struct DirectoryTree {
    root: PathBuf,
    expanded_files: HashSet<PathBuf>,
    expanded_dirs: HashSet<PathBuf>,
}

impl DirectoryTree {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            expanded_files: HashSet::new(),
            expanded_dirs: HashSet::new(),
        }
    }

    /// Expand a file (given relative to the root) and all of its parent
    /// directories, so the renderer walks down to it.
    pub fn expand(&mut self, rel_path: &str) {
        let abs_path = self.root.join(rel_path);
        if !abs_path.is_file() || !abs_path.starts_with(&self.root) {
            return;
        }
        self.expanded_files.insert(abs_path.clone());

        let mut current = abs_path.parent().map(Path::to_path_buf);
        while let Some(dir) = current {
            if dir != self.root && !dir.starts_with(&self.root) {
                break;
            }
            self.expanded_dirs.insert(dir.clone());
            if dir == self.root {
                break;
            }
            current = dir.parent().map(Path::to_path_buf);
        }
    }

    /// Directory contents sorted directories-first, then case-insensitively by name.
    fn list_directory(dir_path: &Path) -> Vec<PathBuf> {
        let Ok(read_dir) = fs::read_dir(dir_path) else {
            return Vec::new();
        };
        let mut contents: Vec<PathBuf> =
            read_dir.filter_map(|entry| entry.ok().map(|e| e.path())).collect();
        contents.sort_by(|a, b| {
            let a_is_dir = a.is_dir();
            let b_is_dir = b.is_dir();
            // `not is_dir` first key → directories (false) come before files (true).
            (!a_is_dir, file_name_lower(a)).cmp(&(!b_is_dir, file_name_lower(b)))
        });
        contents
    }

    fn count_hidden(dir_path: &Path, shown: &[PathBuf]) -> (usize, usize) {
        let shown_set: HashSet<&PathBuf> = shown.iter().collect();
        let mut hidden_files = 0;
        let mut hidden_dirs = 0;
        for item in Self::list_directory(dir_path) {
            if shown_set.contains(&item) {
                continue;
            }
            if item.is_dir() {
                hidden_dirs += 1;
            } else {
                hidden_files += 1;
            }
        }
        (hidden_files, hidden_dirs)
    }

    pub fn display(&self) -> String {
        let mut out = String::new();
        self.display_recursive(&self.root, 0, 0, &mut out);
        out
    }

    fn display_recursive(&self, current: &Path, indent: usize, depth: usize, out: &mut String) {
        if current == self.root {
            let _ = writeln!(out, "{}/", current.display());
        } else {
            let name = file_name_str(current);
            let _ = writeln!(out, "{:indent$}{}/", "", name, indent = indent);
        }

        // Past the top level, only descend into directories we explicitly expanded.
        if depth > 0 && !self.expanded_dirs.contains(current) {
            return;
        }

        let mut shown = Vec::new();
        for item in Self::list_directory(current) {
            let should_show =
                self.expanded_files.contains(&item) || self.expanded_dirs.contains(&item);
            if !should_show {
                continue;
            }
            shown.push(item.clone());
            if item.is_dir() {
                self.display_recursive(&item, indent + 2, depth + 1, out);
            } else {
                let _ = writeln!(out, "{:width$}{}", "", file_name_str(&item), width = indent + 2);
            }
        }

        let (hidden_files, hidden_dirs) = Self::count_hidden(current, &shown);
        if hidden_files > 0 || hidden_dirs > 0 {
            let _ = writeln!(out, "{:width$}...", "", width = indent + 2);
        }
    }
}

fn file_name_str(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn file_name_lower(path: &Path) -> String {
    file_name_str(path).to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::Result;
    use tempfile::TempDir;

    #[test]
    fn renders_expanded_files_and_collapses_rest() -> Result<()> {
        let temp = TempDir::new()?;
        let root = temp.path();
        fs::create_dir(root.join("src"))?;
        fs::write(root.join("src/main.rs"), "fn main() {}\n")?;
        fs::write(root.join("src/hidden.rs"), "\n")?;
        fs::write(root.join("README.md"), "x\n")?;

        let mut tree = DirectoryTree::new(root);
        tree.expand("src/main.rs");
        let display = tree.display();

        assert!(display.contains("src/"));
        assert!(display.contains("main.rs"));
        // hidden.rs and README.md were never expanded → collapsed into "..."
        assert!(display.contains("..."));
        assert!(!display.contains("hidden.rs"));
        Ok(())
    }
}
