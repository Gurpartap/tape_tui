use agent_provider::RunMessage;

use crate::error::SessionStoreError;
use crate::store::SessionStore;

impl SessionStore {
    pub fn replay_leaf(
        &self,
        _target_leaf: Option<&str>,
    ) -> Result<Vec<RunMessage>, SessionStoreError> {
        todo!("implemented in bundle B5")
    }
}
