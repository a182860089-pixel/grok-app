/// Resolve relative media refs to absolute paths that exist on disk.
/// Tries (1) every GROK_HOME copy of the agent session dir (`images/1.jpg`,
/// plus basename fallback `1.jpg` → `images/1.jpg`), then (2) project cwd
/// (skill outputs like `outputs/xhx-media-gen/foo.png`).
/// Skips missing / unsafe paths.
#[tauri::command]
pub async fn session_resolve_relative_media(
    id: String,
    relatives: Vec<String>,
) -> Result<Vec<store::MessageAttachmentStored>, String> {
    let (session_roots, project_root) = resolve_media_search_roots(&id);
    if session_roots.is_empty() && project_root.is_none() {
        return Ok(vec![]);
    }
    let mut roots = session_roots;
    if let Some(project) = project_root {
        roots.push(project);
    }
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for rel in relatives {
        let Some(full) = crate::paths::resolve_relative_media_in_roots(&rel, &roots) else {
            continue;
        };
        // Allow media:// previews for session/project skill outputs (including
        // untrusted project roots that are not in the global path_scope list).
        crate::path_scope::grant_path(&full);
        let path = full.to_string_lossy().to_string();
        if !seen.insert(path.clone()) {
            continue;
        }
        let name = full
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());
        out.push(store::MessageAttachmentStored {
            path,
            name,
            is_dir: false,
        });
    }
    Ok(out)
}

fn resolve_media_search_roots(
    session_id: &str,
) -> (Vec<std::path::PathBuf>, Option<std::path::PathBuf>) {
    let meta = store::load_sessions_index()
        .into_iter()
        .find(|s| s.id == session_id);
    let Some(meta) = meta else {
        return (Vec::new(), None);
    };
    let project_root = meta.project_id.as_ref().and_then(|pid| {
        store::load_projects()
            .into_iter()
            .find(|p| &p.id == pid)
            .map(|p| std::path::PathBuf::from(p.path))
    });
    let session_roots = meta
        .agent_session_id
        .as_deref()
        .map(|agent_sid| {
            let settings = store::load_settings();
            crate::paths::find_all_agent_session_dirs(
                agent_sid,
                project_root
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string())
                    .as_deref(),
                &settings.session_data_mode,
            )
        })
        .unwrap_or_default();
    (session_roots, project_root)
}

fn resolve_session_media_root(session_id: &str) -> Option<String> {
    resolve_media_search_roots(session_id)
        .0
        .into_iter()
        .next()
        .map(|p| p.to_string_lossy().to_string())
}

