use anyhow::{anyhow, Result};
use std::collections::HashMap;
use crate::analyzer::RustAnalyzerClient;

/// Debug log to stderr.
fn dbg_log(msg: &str) {
    eprintln!("[mcp-rlsp debug] {msg}");
}

/// Manages multiple rust-analyzer instances, one per project root.
/// Each client is lazily started on first use for its project path.
pub struct ProjectManager {
    /// Keyed by canonicalized project root path. Each entry holds a
    /// RustAnalyzerClient configured to analyze that specific project.
    clients: HashMap<String, RustAnalyzerClient>,
    /// The currently active project key (canonicalized path). Tool calls
    /// dispatch through this client unless overridden per-call.
    active_project: Option<String>,
}

impl Default for ProjectManager {
    fn default() -> Self { Self::new() }
}

impl ProjectManager {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            active_project: None,
        }
    }

    /// Canonicalize a project path to use as a stable key.
    /// Falls back to the raw input if canonicalization fails (path doesn't exist yet).
    fn canon_key(path: &str) -> String {
        std::fs::canonicalize(path)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.to_string())
    }

    /// Register a project root. Does not start RA immediately — the client
    /// is created and will lazily start on first tool call.
    pub fn add_project(&mut self, project_path: &str) -> Result<String> {
        let key = Self::canon_key(project_path);
        if !self.clients.contains_key(&key) {
            dbg_log(&format!("add_project: registered {key}"));
            self.clients.insert(key.clone(), RustAnalyzerClient::new());
        }
        // If no active project, set this as the default.
        if self.active_project.is_none() {
            self.active_project = Some(key.clone());
            dbg_log(&format!("add_project: auto-set active to {key}"));
        }
        Ok(key)
    }

    /// Switch the active project. Returns error if not registered.
    pub fn set_active(&mut self, project_path: &str) -> Result<()> {
        let key = Self::canon_key(project_path);
        if !self.clients.contains_key(&key) {
            return Err(anyhow!(
                "Project '{project_path}' is not registered. Use add_project first."
            ));
        }
        self.active_project = Some(key.clone());
        dbg_log(&format!("set_active: switched to {key}"));
        Ok(())
    }

    /// Get the active project key, or None if no projects are registered.
    pub fn get_active_key(&self) -> Option<String> {
        self.active_project.clone()
    }

    /// List all registered project paths and which one is active.
    pub fn list_projects(&self) -> Vec<(String, bool)> {
        let mut entries: Vec<(String, bool)> = self
            .clients
            .keys()
            .map(|k| (k.clone(), Some(k.clone()) == self.active_project))
            .collect();
        // Sort for stable output; active project first.
        entries.sort_by(|a, b| {
            let a_active = if a.1 { 0 } else { 1 };
            let b_active = if b.1 { 0 } else { 1 };
            a_active.cmp(&b_active).then_with(|| a.0.cmp(&b.0))
        });
        entries
    }

    /// Get the number of registered projects.
    pub fn project_count(&self) -> usize {
        self.clients.len()
    }

    /// Ensure RA is started for the active project and return a mutable reference
    /// to its client. If no active project, returns error (no default fallback).
    pub async fn get_active_client_mut(&mut self) -> Result<&mut RustAnalyzerClient> {
        let key = self.active_project.clone().ok_or_else(|| {
            anyhow!("No active project set. Register a project with add_project first.")
        })?;
        // Re-borrow after moving key out of self.
        let client = self.clients.get_mut(&key).ok_or_else(|| {
            anyhow!(
                "Active project '{key}' not found in clients map (internal error)"
            )
        })?;

        if !client.is_initialized() {
            dbg_log(&format!("get_active_client_mut: lazily starting RA for {key}"));
            client.ensure_started_with_project(&key).await?;
        }
        Ok(client)
    }

    /// Given a file path, determine which registered project it belongs to
    /// (by checking if the canonicalized file path starts with a project key).
    /// Returns the matching project key, or None.
    fn find_project_for_file(&self, file_path: &str) -> Option<String> {
        let canon_file = std::fs::canonicalize(file_path)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| file_path.to_string());
        for key in self.clients.keys() {
            if canon_file.starts_with(key.as_str()) {
                return Some(key.clone());
            }
        }
        None
    }

    /// Ensure the right RA client is started and returned. If a tool call provides
    /// a file_path, auto-switch to the project that contains it (if registered).
    pub async fn get_client_for_file(
        &mut self,
        file_path: Option<&str>,
    ) -> Result<&mut RustAnalyzerClient> {
        // Auto-detect project from file path if possible.
        if let Some(path) = file_path {
            if let Some(proj_key) = self.find_project_for_file(path) {
                dbg_log(&format!(
                    "get_client_for_file: {path} belongs to registered project {proj_key}"
                ));
                self.active_project = Some(proj_key.clone());
                let client = self.clients.get_mut(&proj_key).ok_or_else(|| {
                    anyhow!("Project '{proj_key}' not found in clients map")
                })?;
                if !client.is_initialized() {
                    dbg_log(&format!(
                        "get_client_for_file: lazily starting RA for {proj_key}"
                    ));
                    client.ensure_started_with_project(&proj_key).await?;
                }
                return Ok(client);
            }
        }

        self.get_active_client_mut().await
    }
}
