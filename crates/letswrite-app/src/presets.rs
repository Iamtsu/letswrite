//! Built-in preset prompts for the assistant column.
//!
//! Each preset has a stable id (used by `AgentInput::preset` and persisted
//! later when threads land in the DB) and a short user-facing label.
//! Selecting a preset fills the composer with the preset's prompt — the
//! user can edit it freely before sending.
//!
//! A future "User-editable presets" task adds the ability to add/edit/
//! remove presets; the data is structured to support that without
//! breaking the wire shape.

#[derive(Debug, Clone)]
pub(crate) struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    pub prompt: &'static str,
}

/// All built-in presets in display order.
pub(crate) const BUILT_INS: &[Preset] = &[
    Preset {
        id: "critique",
        label: "Critique scene",
        prompt: "Critique the selected scene. Be concrete — quote the prose you're reacting to. \
                 What's working? What's blunted? Where does a reader's attention slip?",
    },
    Preset {
        id: "continuity",
        label: "Continuity check",
        prompt: "Check this scene for continuity errors with the characters and locations \
                 listed above. Are anyone's traits, motivations, or stated facts contradicted? \
                 Flag specific lines.",
    },
    Preset {
        id: "pacing",
        label: "Pacing notes",
        prompt: "Comment on the pacing of this scene. Where does it slow down or rush? \
                 Quote the beats that drag or jump.",
    },
    Preset {
        id: "voice",
        label: "Voice check",
        prompt: "Pick one character present in this scene and tell me where their voice \
                 slips — dialogue or internal narration that doesn't match how they speak \
                 elsewhere in the project. Quote the line and explain.",
    },
    Preset {
        id: "opening",
        label: "Sharper opening",
        prompt: "Suggest three alternative opening lines for this scene that are sharper \
                 than the current one. Keep the same tone. Number them 1–3 and briefly \
                 say why each works.",
    },
    Preset {
        id: "summarise",
        label: "Summarise",
        prompt: "Summarise this scene in two sentences as if writing a back-cover blurb. \
                 Make it specific, not generic.",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_ids_are_unique() {
        let mut seen: Vec<&str> = Vec::new();
        for p in BUILT_INS {
            assert!(!seen.contains(&p.id), "duplicate preset id: {}", p.id);
            seen.push(p.id);
        }
    }

    #[test]
    fn preset_ids_are_kebab_case() {
        for p in BUILT_INS {
            assert!(
                p.id.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "preset id should be kebab-case: {}",
                p.id
            );
            assert!(!p.id.is_empty());
        }
    }

    #[test]
    fn preset_labels_and_prompts_are_present() {
        for p in BUILT_INS {
            assert!(!p.label.is_empty(), "label empty for {}", p.id);
            assert!(!p.prompt.is_empty(), "prompt empty for {}", p.id);
        }
    }
}
