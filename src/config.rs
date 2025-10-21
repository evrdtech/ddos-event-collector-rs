use once_cell::sync::Lazy;
use std::sync::Mutex;

pub static NODE_INFO: Lazy<Mutex<NodeInfo>> = Lazy::new(|| {
    Mutex::new(NodeInfo {
        node_id: None,
    })
});

#[derive(Debug)]
pub struct NodeInfo {
    pub node_id: Option<String>,
}

impl NodeInfo {
    pub async fn load_from_db(pool: &sqlx::SqlitePool) -> Result<(), sqlx::Error> {
        let mut lock = NODE_INFO.lock().unwrap();
        let row: Option<(String,)> = sqlx::query_as("SELECT value FROM config WHERE key = 'node_id'")
            .fetch_optional(pool)
            .await?;
        lock.node_id = row.map(|r| r.0);
        Ok(())
    }

    #[allow(dead_code)] // Suppress warning for unused method
    pub fn set_node_id(&mut self, node_id: Option<String>) {
        self.node_id = node_id;
    }

    pub fn get_node_id(&self) -> Option<String> {
        self.node_id.clone()
    }

    pub fn is_node_id_set(&self) -> bool {
        self.node_id.is_some()
    }
}