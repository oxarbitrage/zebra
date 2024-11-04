//!
//!

/// Define JSON-RPC request and response structures
#[derive(Debug, Clone, serde::Deserialize)]
pub struct JsonRpcRequest {
    jsonrpc: String,
    id: String,
    method: String,
    params: Vec<serde_json::Value>,
}

impl JsonRpcRequest {
    ///
    pub fn method(&self) -> &str {
        &self.method
    }

    ///
    pub fn params(&self) -> &[serde_json::Value] {
        &self.params
    }

    ///
    pub fn id(&self) -> &str {
        &self.id
    }
}

///
#[derive(serde::Serialize)]
pub struct JsonRpcResponse {
    //jsonrpc: String,
    result: serde_json::Value,
    id: String,
    //error: Option<String>,
}

impl JsonRpcResponse {
    ///
    pub fn new(result: serde_json::Value, id: String) -> Self {
        Self { result, id }
    }
}

///
#[derive(serde::Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}
