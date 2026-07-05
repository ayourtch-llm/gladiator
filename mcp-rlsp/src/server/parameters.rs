use rmcp::schemars;

// Parameter structs for tools
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FindDefinitionParams {
    pub file_path: String,
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FindReferencesParams {
    pub file_path: String,
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetDiagnosticsParams {
    pub file_path: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct WorkspaceSymbolsParams {
    pub query: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RenameSymbolParams {
    pub file_path: String,
    pub line: u32,
    pub character: u32,
    pub new_name: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExtractFunctionParams {
    pub file_path: String,
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
    pub function_name: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct InlineFunctionParams {
    pub file_path: String,
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct OrganizeImportsParams {
    pub file_path: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetTypeHierarchyParams {
    pub file_path: String,
    pub line: u32,
    pub character: u32,
}
