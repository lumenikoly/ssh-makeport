use crate::model::{validate_target, MAX_TASKS};
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub description: String,
}

// Deliberately not a GNU Make evaluator. Never execute `make -p` or $(shell ...)
// merely to list tasks. Explicit annotations first; .PHONY is the fallback.
pub fn discover(source: &str) -> Vec<Target> {
    let mut annotated = Vec::new();
    let mut phony = Vec::new();
    let mut in_define = 0_u32;
    let mut continued = false;
    for raw in source.lines() {
        let text = raw.trim();
        let skip_continuation = continued;
        continued = raw.trim_end().ends_with('\\');
        if raw.starts_with('\t') || skip_continuation { continue; }
        if text.starts_with("define ") || text.starts_with("define\t") {
            in_define += 1;
            continue;
        }
        if text == "endef" {
            in_define = in_define.saturating_sub(1);
            continue;
        }
        if in_define != 0 || text.starts_with('#') { continue; }
        let Some((left, right)) = text.split_once(':') else { continue; };
        if left.trim() == ".PHONY" {
            for name in right.split('#').next().unwrap_or("").split_whitespace() {
                if validate_target(name).is_ok() { phony.push(Target { name: name.into(), description: String::new() }); }
            }
            continue;
        }
        if right.trim_start().starts_with('=') || left.contains('=') { continue; }
        let Some((_, description)) = right.split_once("##") else { continue; };
        // Pattern rules, special dot targets and variable expansion are not UI tasks.
        for name in left.split_whitespace() {
            if !name.starts_with('.') && validate_target(name).is_ok() {
                annotated.push(Target { name: name.into(), description: description.trim().chars().take(300).collect() });
            }
        }
    }
    let targets = if annotated.is_empty() { phony } else { annotated };
    let mut seen = HashSet::new();
    targets.into_iter().filter(|t| seen.insert(t.name.clone())).take(MAX_TASKS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn annotated_targets_win() {
        let result = discover(".PHONY: dev internal\ndev: ## Start server\n\tpnpm dev\ninternal:\n");
        assert_eq!(result, vec![Target { name: "dev".into(), description: "Start server".into() }]);
    }
    #[test]
    fn phony_fallback_is_deduplicated() {
        let result = discover(".PHONY: dev test dev\n");
        assert_eq!(result.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["dev", "test"]);
    }
    #[test]
    fn ignores_dynamic_rules_recipes_and_defines() {
        let source = "define X\nwrong: ## no\nendef\n%.o: ## no\n$(TARGET): ## no\n\techo x: ## no\nV := a ## no\nok: ## yes\n";
        assert_eq!(discover(source).iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["ok"]);
    }
    #[test]
    fn multiple_explicit_targets_are_supported() {
        assert_eq!(discover("build test: ## Run\n").len(), 2);
    }
    #[test]
    fn ordinary_unannotated_rules_are_not_guessed() {
        assert!(discover("output.o: input.c\n\tcc input.c\n").is_empty());
    }
}
