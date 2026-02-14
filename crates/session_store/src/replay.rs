use std::collections::HashSet;

use agent_provider::RunMessage;

use crate::error::SessionStoreError;
use crate::schema::SessionEntryKind;
use crate::store::SessionStore;

impl SessionStore {
    pub fn replay_leaf(
        &self,
        target_leaf: Option<&str>,
    ) -> Result<Vec<RunMessage>, SessionStoreError> {
        let start_leaf_id = match target_leaf {
            Some(target) => target.to_string(),
            None => match &self.current_leaf_id {
                Some(current) => current.clone(),
                None => return Ok(Vec::new()),
            },
        };

        if !self.index_by_id.contains_key(&start_leaf_id) {
            return Err(SessionStoreError::UnknownLeafId {
                path: self.path.clone(),
                leaf_id: start_leaf_id,
            });
        }

        let mut chain_indices: Vec<usize> = Vec::new();
        let mut visited = HashSet::new();
        let mut cursor = Some(start_leaf_id.clone());

        while let Some(entry_id) = cursor {
            if !visited.insert(entry_id.clone()) {
                return Err(SessionStoreError::ReplayCycle {
                    path: self.path.clone(),
                    leaf_id: start_leaf_id,
                });
            }

            let index = self.index_by_id.get(&entry_id).copied().ok_or_else(|| {
                SessionStoreError::UnknownLeafId {
                    path: self.path.clone(),
                    leaf_id: entry_id.clone(),
                }
            })?;
            let entry = &self.entries[index];
            chain_indices.push(index);
            cursor = entry.parent_id.clone();
        }

        chain_indices.reverse();

        let mut messages = Vec::with_capacity(chain_indices.len());
        for index in chain_indices {
            messages.push(entry_to_run_message(&self.entries[index]));
        }

        Ok(messages)
    }
}

fn entry_to_run_message(entry: &crate::schema::SessionEntry) -> RunMessage {
    match &entry.kind {
        SessionEntryKind::UserText { text } => RunMessage::UserText { text: text.clone() },
        SessionEntryKind::AssistantText { text } => {
            RunMessage::AssistantText { text: text.clone() }
        }
        SessionEntryKind::ToolCall {
            call_id,
            tool_name,
            arguments,
        } => RunMessage::ToolCall {
            call_id: call_id.clone(),
            tool_name: tool_name.clone(),
            arguments: arguments.clone(),
        },
        SessionEntryKind::ToolResult {
            call_id,
            tool_name,
            content,
            is_error,
        } => RunMessage::ToolResult {
            call_id: call_id.clone(),
            tool_name: tool_name.clone(),
            content: content.clone(),
            is_error: *is_error,
        },
    }
}
