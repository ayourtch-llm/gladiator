use std::collections::HashMap;

pub struct ToolStream {
    pub tools: HashMap<usize, ToolAccumulator>,
}

pub struct ToolAccumulator {
    pub id: String,
    pub name: String,
    pub input: String,
}

impl ToolAccumulator {
    pub fn new() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            input: String::new(),
        }
    }

    pub fn push_name(&mut self, name: &str) {
        self.name.push_str(name);
    }

    pub fn push_input(&mut self, input: &str) {
        self.input.push_str(input);
    }

    pub fn push_id(&mut self, id: &str) {
        self.id.push_str(id);
    }
}

impl ToolStream {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn append_or_start(
        &mut self,
        index: usize,
        id: &str,
        name: &str,
        input: &str,
    ) {
        let entry = self.tools.entry(index).or_insert_with(ToolAccumulator::new);
        if !id.is_empty() {
            entry.push_id(id);
        }
        if !name.is_empty() {
            entry.push_name(name);
        }
        if !input.is_empty() {
            entry.push_input(input);
        }
    }

    pub fn finish_all(&self) -> Vec<serde_json::Value> {
        let mut result = Vec::new();
        for (_idx, entry) in &self.tools {
            let full_tc = serde_json::json!({
                "id": entry.id,
                "type": "function",
                "function": {
                    "name": entry.name,
                    "arguments": entry.input,
                }
            });
            result.push(full_tc);
        }
        result
    }

    pub fn finish_one(&self, index: usize) -> Option<serde_json::Value> {
        self.tools.get(&index).map(|entry| {
            serde_json::json!({
                "id": entry.id,
                "type": "function",
                "function": {
                    "name": entry.name,
                    "arguments": entry.input,
                }
            })
        })
    }
}
