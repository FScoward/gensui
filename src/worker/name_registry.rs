use std::collections::HashMap;
use thiserror::Error;
use crate::worker::WorkerId;

#[derive(Debug, Error)]
pub enum NameConflictError {
    #[error("Worker name '{0}' is already in use by worker {1:?}")]
    AlreadyExists(String, WorkerId),
}

pub struct NameRegistry {
    name_to_id: HashMap<String, WorkerId>,
}

impl NameRegistry {
    pub fn new() -> Self {
        Self {
            name_to_id: HashMap::new(),
        }
    }

    pub fn register(&mut self, name: String, id: WorkerId) -> Result<(), NameConflictError> {
        if let Some(existing_id) = self.name_to_id.get(&name) {
            return Err(NameConflictError::AlreadyExists(name, *existing_id));
        }
        self.name_to_id.insert(name, id);
        Ok(())
    }

    pub fn unregister(&mut self, name: &str) {
        self.name_to_id.remove(name);
    }

    pub fn rename(
        &mut self,
        old_name: &str,
        new_name: String,
        id: WorkerId,
    ) -> Result<(), NameConflictError> {
        // Check if new name is available
        if let Some(existing_id) = self.name_to_id.get(&new_name) {
            if *existing_id != id {
                return Err(NameConflictError::AlreadyExists(new_name, *existing_id));
            }
        }

        // Remove old name and add new name
        self.name_to_id.remove(old_name);
        self.name_to_id.insert(new_name, id);
        Ok(())
    }

    pub fn is_available(&self, name: &str) -> bool {
        !self.name_to_id.contains_key(name)
    }

    pub fn get_id(&self, name: &str) -> Option<WorkerId> {
        self.name_to_id.get(name).copied()
    }
}

impl Default for NameRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name_registry_uniqueness() {
        let mut registry = NameRegistry::new();
        let id1 = WorkerId(1);
        let id2 = WorkerId(2);

        assert!(registry.register("worker1".to_string(), id1).is_ok());
        assert!(registry.register("worker1".to_string(), id2).is_err());
    }

    #[test]
    fn test_name_registry_rename() {
        let mut registry = NameRegistry::new();
        let id = WorkerId(1);

        registry.register("old_name".to_string(), id).unwrap();
        assert!(registry.rename("old_name", "new_name".to_string(), id).is_ok());
        assert!(registry.is_available("old_name"));
        assert!(!registry.is_available("new_name"));
        assert_eq!(registry.get_id("new_name"), Some(id));
    }

    #[test]
    fn test_name_registry_rename_conflict() {
        let mut registry = NameRegistry::new();
        let id1 = WorkerId(1);
        let id2 = WorkerId(2);

        registry.register("worker1".to_string(), id1).unwrap();
        registry.register("worker2".to_string(), id2).unwrap();

        // Try to rename worker1 to worker2 (should fail)
        assert!(registry.rename("worker1", "worker2".to_string(), id1).is_err());
    }

    #[test]
    fn test_name_registry_rename_same_name() {
        let mut registry = NameRegistry::new();
        let id = WorkerId(1);

        registry.register("worker1".to_string(), id).unwrap();
        // Renaming to the same name should succeed
        assert!(registry.rename("worker1", "worker1".to_string(), id).is_ok());
    }

    #[test]
    fn test_name_registry_unregister() {
        let mut registry = NameRegistry::new();
        let id = WorkerId(1);

        registry.register("worker1".to_string(), id).unwrap();
        assert!(!registry.is_available("worker1"));

        registry.unregister("worker1");
        assert!(registry.is_available("worker1"));
    }

    #[test]
    fn test_name_registry_is_available() {
        let mut registry = NameRegistry::new();
        let id = WorkerId(1);

        assert!(registry.is_available("worker1"));
        registry.register("worker1".to_string(), id).unwrap();
        assert!(!registry.is_available("worker1"));
    }

    #[test]
    fn test_name_registry_get_id() {
        let mut registry = NameRegistry::new();
        let id = WorkerId(1);

        assert_eq!(registry.get_id("worker1"), None);
        registry.register("worker1".to_string(), id).unwrap();
        assert_eq!(registry.get_id("worker1"), Some(id));
    }
}
