use std::{collections::HashMap, sync::Mutex};

use crate::Result;

pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>>;
    fn set(&self, key: &str, value: &str) -> Result<()>;
    fn delete(&self, key: &str) -> Result<()>;
}

#[derive(Default)]
pub struct InMemorySecretStore {
    values: Mutex<HashMap<String, String>>,
}

impl InMemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for InMemorySecretStore {
    fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .values
            .lock()
            .expect("in-memory secret store poisoned")
            .get(key)
            .cloned())
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        self.values
            .lock()
            .expect("in-memory secret store poisoned")
            .insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<()> {
        self.values
            .lock()
            .expect("in-memory secret store poisoned")
            .remove(key);
        Ok(())
    }
}
