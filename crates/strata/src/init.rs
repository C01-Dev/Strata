//! `strata init`: detect the repo's setup and write `strata.toml`.

use std::fs;
use strata_core::config::{self, Config};
use strata_core::{Result, detect};

pub fn run() -> Result<()> {
    let root = strata_git::repo_root(&std::env::current_dir()?)?;
    let found = detect::detect(&root);
    let path = root.join(config::FILE);
    if path.exists() {
        Config::load(&root)?;
        println!("kept {}", path.display());
    } else {
        let folder = root.file_name().unwrap_or_default().to_string_lossy();
        let base = strata_git::default_base(&root)?;
        let text = config::template(
            &base,
            &format!("../.strata/{folder}"),
            found.dev_command.as_deref(),
            &dns_label(&folder),
        );
        fs::write(&path, text)?;
        println!("wrote {}", path.display());
    }
    println!(
        "package manager: {}",
        found.package_manager.map_or("none found", |pm| pm.name())
    );
    println!(
        "typescript: {}",
        found.typescript.as_deref().unwrap_or("not installed")
    );
    match found.checker_off_reason() {
        Some(reason) => println!("checker: off. {reason}"),
        None => println!("checker: on"),
    }
    println!(
        "dev command: {}",
        found
            .dev_command
            .as_deref()
            .unwrap_or("none (set dev.command in strata.toml)")
    );
    Ok(())
}

/// Folder name turned into a hostname label for `dev.app`.
fn dns_label(name: &str) -> String {
    let label: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let label = label.trim_matches('-');
    let label = &label[..label.len().min(63)];
    if label.is_empty() {
        "app".into()
    } else {
        label.trim_end_matches('-').into()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dns_label_from_folder() {
        assert_eq!(super::dns_label("My_App.v2"), "my-app-v2");
        assert_eq!(super::dns_label("日本"), "app");
        assert!(strata_core::config::is_dns_label(&super::dns_label(
            &format!("{}-x", "a".repeat(62))
        )));
    }
}
