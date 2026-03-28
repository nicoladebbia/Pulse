use std::path::PathBuf;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfile {
    pub projects: Vec<ProjectInfo>,
    pub tech_stacks: Vec<String>,
    pub domains: Vec<String>,
    pub location: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub name: String,
    pub path: String,
    pub tech_stack: Vec<String>,
    pub domain: String,
}

pub fn scan_projects_dir() -> UserProfile {
    let projects_dir = dirs::home_dir()
        .unwrap_or_default()
        .join("Projects");

    let mut projects = Vec::new();
    let mut tech_stacks = std::collections::HashSet::new();
    let mut domains = std::collections::HashSet::new();

    if let Ok(entries) = std::fs::read_dir(&projects_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            let (stack, domain) = detect_project_info(&path);

            for t in &stack {
                tech_stacks.insert(t.clone());
            }
            domains.insert(domain.clone());

            projects.push(ProjectInfo {
                name,
                path: path.to_string_lossy().to_string(),
                tech_stack: stack,
                domain,
            });
        }
    }

    UserProfile {
        projects,
        tech_stacks: tech_stacks.into_iter().collect(),
        domains: domains.into_iter().collect(),
        location: "Miami Beach, FL".to_string(),
    }
}

fn detect_project_info(path: &PathBuf) -> (Vec<String>, String) {
    let mut stack = Vec::new();
    let mut domain = "general".to_string();

    if path.join("package.json").exists() {
        stack.push("Node.js".to_string());
    }
    if path.join("Cargo.toml").exists() {
        stack.push("Rust".to_string());
    }
    if path.join("pyproject.toml").exists() || path.join("requirements.txt").exists() {
        stack.push("Python".to_string());
    }
    if path.join("Package.swift").exists() {
        stack.push("Swift".to_string());
    }

    // Detect domain from CLAUDE.md or README.md
    for file in ["CLAUDE.md", "README.md"] {
        if let Ok(content) = std::fs::read_to_string(path.join(file)) {
            let lower = content.to_lowercase();
            if lower.contains("ai") || lower.contains("machine learning") || lower.contains("llm") {
                domain = "AI/ML".to_string();
            } else if lower.contains("betting") || lower.contains("prediction") || lower.contains("serie a") {
                domain = "Sports Analytics".to_string();
            } else if lower.contains("nutrition") || lower.contains("health") {
                domain = "Health".to_string();
            } else if lower.contains("fintech") || lower.contains("finance") {
                domain = "Fintech".to_string();
            } else if lower.contains("shopify") || lower.contains("e-commerce") {
                domain = "E-commerce".to_string();
            }
            break;
        }
    }

    (stack, domain)
}
