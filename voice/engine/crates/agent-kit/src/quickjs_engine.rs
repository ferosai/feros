use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::Client;
use rquickjs::{CatchResultExt, Context, Ctx, Function, Object, Runtime, Value as JsValue};
use secrecy::ExposeSecret;
use tokio::runtime::Handle;
use tracing::{debug, info, warn};

use crate::agent_backends::{SecretMap, SharedSecretMap};
use crate::swarm::ToolDef;
// ── Tool Errors ─────────────────────────────────────────────────

/// Errors that can occur during tool execution.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("Tool not found: {0}")]
    NotFound(String),

    #[error("Invalid arguments for '{name}': {reason}")]
    InvalidArguments { name: String, reason: String },

    #[error("Script error in '{name}': {reason}")]
    ScriptError { name: String, reason: String },
}

/// Maximum bytes per file_write call (1 MB).
const MAX_FILE_SIZE: usize = 1_048_576;

/// Maximum number of files allowed in the sandbox.
const MAX_SANDBOX_FILES: usize = 100;

/// Default timeout for script execution (1 minute).
const DEFAULT_SCRIPT_TIMEOUT: Duration = Duration::from_secs(60);

/// Sandboxed QuickJS script engine with HTTP and file I/O bindings.
///
/// Thread-safe (`Send + Sync`) — safe to wrap in `Arc` for use from
/// `spawn_blocking` tasks.
pub struct QuickJsToolEngine {
    rt: Runtime,
    tools: HashMap<String, ToolDef>,
    sandbox: Option<PathBuf>,
    secrets: SharedSecretMap,
    http_client: Arc<Client>,
    script_timeout: Duration,
}

impl QuickJsToolEngine {
    pub fn new(sandbox: Option<PathBuf>, secrets: SharedSecretMap) -> Self {
        let rt = Runtime::new().unwrap();
        rt.set_memory_limit(20 * 1024 * 1024); // 20 MB limit
        rt.set_max_stack_size(1024 * 1024); // 1 MB limit

        let http_client = Arc::new(
            Client::builder()
                .timeout(Duration::from_secs(10))
                .connect_timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
        );

        Self {
            rt,
            tools: HashMap::new(),
            sandbox,
            secrets,
            http_client,
            script_timeout: DEFAULT_SCRIPT_TIMEOUT,
        }
    }

    pub fn with_tools(tools: HashMap<String, ToolDef>) -> Self {
        let mut engine = Self::new(None, Arc::new(std::sync::RwLock::new(SecretMap::new())));
        engine.tools = tools;
        engine
    }

    pub fn with_tools_and_sandbox(
        tools: HashMap<String, ToolDef>,
        sandbox: PathBuf,
        secrets: SharedSecretMap,
    ) -> Self {
        let mut engine = Self::new(Some(sandbox), secrets);
        engine.tools = tools;
        engine
    }

    pub fn register(&mut self, name: String, tool: ToolDef) {
        info!("[js] Registered tool: '{}'", name);
        self.tools.insert(name, tool);
    }

    pub fn get(&self, name: &str) -> Option<&ToolDef> {
        self.tools.get(name)
    }

    pub fn sandbox_path(&self) -> Option<&PathBuf> {
        self.sandbox.as_ref()
    }

    pub fn execute(&self, name: &str, arguments_json: &str) -> Result<String, ToolError> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| ToolError::NotFound(name.to_string()))?;

        // Parse args quickly using minimal serde_json to check validity first.
        let args: serde_json::Value = if arguments_json.is_empty() || arguments_json == "{}" {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_str(arguments_json).map_err(|e| ToolError::InvalidArguments {
                name: name.to_string(),
                reason: e.to_string(),
            })?
        };

        debug!(
            "[js] Executing tool '{}' with args: {}",
            name, arguments_json
        );

        // Build execution context
        let ctx = Context::full(&self.rt).map_err(|e| ToolError::ScriptError {
            name: name.to_string(),
            reason: format!("Failed to create context: {:?}", e),
        })?;

        let http_client = self.http_client.clone();

        let sandbox_path = self.sandbox.clone();

        // Install interrupt handler for timeout enforcement.
        // NOTE: The handler is set on the shared Runtime. `execute()` is NOT
        // re-entrant — concurrent calls clobber each other's deadlines.
        // QuickJS's internal mutex serializes execution, so the worst-case
        // scenario is a tool inherits a later execution's deadline. Given
        // typical usage, this is an acceptable low-severity race.
        let deadline = Instant::now() + self.script_timeout;
        self.rt
            .set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)));

        let result = ctx.with(|ctx| {
            let globals = ctx.globals();

            // 1. Setup execution environment
            let secrets_ref = self.secrets.clone();
            globals
                .set(
                    "secret",
                    Function::new(ctx.clone(), move |key: String| -> String {
                        match secrets_ref.read() {
                            Ok(map) => match map.get(&key) {
                                Some(val) => {
                                    info!(secret_key = %key, "secret() accessed");
                                    val.expose_secret().to_string()
                                }
                                None => {
                                    warn!(secret_key = %key, "secret() miss — returning empty string");
                                    String::new()
                                }
                            },
                            Err(_) => {
                                warn!(secret_key = %key, "secret() lock poisoned — returning empty string");
                                String::new()
                            }
                        }
                    }),
                )
                .unwrap();

            globals
                .set(
                    "log",
                    Function::new(ctx.clone(), |msg: String| {
                        info!("[js] {}", msg);
                    }),
                )
                .unwrap();

            // 2. Setup HTTP methods
            setup_http_globals(ctx.clone(), &globals, http_client.clone());

            // 3. Setup File I/O methods
            setup_file_io_globals(ctx.clone(), &globals, sandbox_path);

            // 4. Inject arguments directly into the global execution context as variables
            if let serde_json::Value::Object(map) = &args {
                for (k, v) in map {
                    let v_json =
                        serde_json::to_string(v).map_err(|e| ToolError::InvalidArguments {
                            name: name.to_string(),
                            reason: e.to_string(),
                        })?;
                    let jsv: JsValue =
                        ctx.json_parse(v_json)
                            .map_err(|e| ToolError::InvalidArguments {
                                name: name.to_string(),
                                reason: format!("JSON parse error: {:?}", e),
                            })?;
                    globals
                        .set(k.as_str(), jsv)
                        .map_err(|e| ToolError::ScriptError {
                            name: name.to_string(),
                            reason: format!("Failed to inject variable {}: {:?}", k, e),
                        })?;
                }
            }

            // 5. Wrap tool script in an IIFE so `return something;` operates correctly.
            let script = format!("(function() {{\n{}\n}})()", tool.script);
            let res: Result<JsValue, rquickjs::Error> = ctx.eval(script);

            match res.catch(&ctx) {
                Ok(val) => {
                    if let Some(s) = val.as_string() {
                        Ok(s.to_string().unwrap_or_default())
                    } else if val.is_object() || val.is_array() {
                        let json_obj: Object =
                            globals.get("JSON").map_err(|e| ToolError::ScriptError {
                                name: name.to_string(),
                                reason: format!("Failed to get JSON global: {:?}", e),
                            })?;
                        let stringify: Function =
                            json_obj
                                .get("stringify")
                                .map_err(|e| ToolError::ScriptError {
                                    name: name.to_string(),
                                    reason: format!("Failed to get JSON.stringify: {:?}", e),
                                })?;
                        let json_str: String =
                            stringify.call((val,)).map_err(|e| ToolError::ScriptError {
                                name: name.to_string(),
                                reason: format!("JSON.stringify failed: {:?}", e),
                            })?;
                        Ok(json_str)
                    } else if val.is_null() || val.is_undefined() {
                        Ok(String::new())
                    } else {
                        let string_fn: Function =
                            globals.get("String").map_err(|e| ToolError::ScriptError {
                                name: name.to_string(),
                                reason: format!("Failed to get String global: {:?}", e),
                            })?;
                        let s: String =
                            string_fn.call((val,)).map_err(|e| ToolError::ScriptError {
                                name: name.to_string(),
                                reason: format!("String conversion failed: {:?}", e),
                            })?;
                        Ok(s)
                    }
                }
                Err(e) => Err(ToolError::ScriptError {
                    name: name.to_string(),
                    reason: format!("Execution failed: {:?}", e),
                }),
            }
        });

        // Trace and return
        let result_str = result?;
        let end = result_str.floor_char_boundary(result_str.len().min(100));
        info!("[js] Tool '{}' result: {}…", name, &result_str[..end]);
        Ok(result_str)
    }
}

// ── Globals Setup ───────────────────────────────────────────────
// The `.unwrap()` calls in setup functions are safe: they set properties on
// a freshly-created global object, which cannot fail in QuickJS.

fn setup_http_globals<'js>(ctx: Ctx<'js>, globals: &Object<'js>, client: Arc<Client>) {
    let cl = client.clone();
    globals
        .set(
            "http_get",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, url: String| -> Object<'js> {
                    http_call_sync(ctx, &cl, "GET", &url, None, None)
                },
            ),
        )
        .unwrap();

    let cl = client.clone();
    globals
        .set(
            "http_post",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, url: String, body: Option<JsValue<'js>>| -> Object<'js> {
                    http_call_sync(ctx, &cl, "POST", &url, body, None)
                },
            ),
        )
        .unwrap();

    let cl = client.clone();
    globals
        .set(
            "http_put",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, url: String, body: Option<JsValue<'js>>| -> Object<'js> {
                    http_call_sync(ctx, &cl, "PUT", &url, body, None)
                },
            ),
        )
        .unwrap();

    let cl = client.clone();
    globals
        .set(
            "http_delete",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, url: String| -> Object<'js> {
                    http_call_sync(ctx, &cl, "DELETE", &url, None, None)
                },
            ),
        )
        .unwrap();

    // With Headers variants
    let cl = client.clone();
    globals
        .set(
            "http_get_h",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, url: String, hdrs: Object<'js>| -> Object<'js> {
                    http_call_sync(ctx, &cl, "GET", &url, None, Some(hdrs))
                },
            ),
        )
        .unwrap();

    let cl = client.clone();
    globals
        .set(
            "http_post_h",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>,
                      url: String,
                      body: Option<JsValue<'js>>,
                      hdrs: Object<'js>|
                      -> Object<'js> {
                    http_call_sync(ctx, &cl, "POST", &url, body, Some(hdrs))
                },
            ),
        )
        .unwrap();

    let cl = client.clone();
    globals
        .set(
            "http_put_h",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>,
                      url: String,
                      body: Option<JsValue<'js>>,
                      hdrs: Object<'js>|
                      -> Object<'js> {
                    http_call_sync(ctx, &cl, "PUT", &url, body, Some(hdrs))
                },
            ),
        )
        .unwrap();

    let cl = client.clone();
    globals
        .set(
            "http_delete_h",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, url: String, hdrs: Object<'js>| -> Object<'js> {
                    http_call_sync(ctx, &cl, "DELETE", &url, None, Some(hdrs))
                },
            ),
        )
        .unwrap();
}

fn setup_file_io_globals<'js>(ctx: Ctx<'js>, globals: &Object<'js>, sandbox: Option<PathBuf>) {
    let sb = sandbox.clone();
    globals
        .set(
            "file_read",
            Function::new(ctx.clone(), move |path: String| -> String {
                sandbox_file_read(&sb, &path)
            }),
        )
        .unwrap();

    let sb = sandbox.clone();
    globals
        .set(
            "file_write",
            Function::new(
                ctx.clone(),
                move |path: String, content: String| -> String {
                    sandbox_file_write(&sb, &path, &content)
                },
            ),
        )
        .unwrap();

    let sb = sandbox.clone();
    globals
        .set(
            "file_exists",
            Function::new(ctx.clone(), move |path: String| -> bool {
                sandbox_file_exists(&sb, &path)
            }),
        )
        .unwrap();

    let sb = sandbox.clone();
    globals
        .set(
            "file_list",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>| -> rquickjs::Array<'js> {
                sandbox_file_list(ctx, &sb)
            }),
        )
        .unwrap();
}

// ── File I/O Inner Helpers ─────────────────────────────────────

fn resolve_sandbox_path(sandbox: &std::path::Path, relative: &str) -> Option<PathBuf> {
    if relative.is_empty() {
        return None;
    }
    let rel_path = std::path::Path::new(relative);
    if rel_path.is_absolute() {
        return None;
    }

    for component in rel_path.components() {
        match component {
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => return None,
            _ => {}
        }
    }

    let full_path = sandbox.join(rel_path);
    if let Some(parent) = full_path.parent() {
        if parent.exists() {
            if let Ok(canonical_parent) = parent.canonicalize() {
                if let Ok(canonical_sandbox) = sandbox.canonicalize() {
                    if !canonical_parent.starts_with(&canonical_sandbox) {
                        return None;
                    }
                }
            }
        }
    }
    Some(full_path)
}

fn sandbox_file_read(sandbox: &Option<PathBuf>, path: &str) -> String {
    let Some(sb) = sandbox else {
        return "Error: file I/O not enabled".to_string();
    };
    let Some(full_path) = resolve_sandbox_path(sb, path) else {
        return format!("Error: invalid path '{}'", path);
    };
    std::fs::read_to_string(&full_path)
        .unwrap_or_else(|e| format!("Error reading '{}': {}", path, e))
}

/// Count all files recursively under `dir`.
/// Unlike the Rhai engine (top-level only), this counts nested files too,
/// giving a more accurate limit check when scripts create subdirectories.
fn count_files_recursive(dir: &std::path::Path) -> std::io::Result<usize> {
    let mut count = 0;
    if dir.is_dir() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                count += count_files_recursive(&entry.path())?;
            } else {
                count += 1;
            }
        }
    }
    Ok(count)
}

fn sandbox_file_write(sandbox: &Option<PathBuf>, path: &str, content: &str) -> String {
    let Some(sb) = sandbox else {
        return "Error: file I/O not enabled".to_string();
    };
    if content.len() > MAX_FILE_SIZE {
        return format!(
            "Error: content too large ({} bytes, max {})",
            content.len(),
            MAX_FILE_SIZE
        );
    }
    let Some(full_path) = resolve_sandbox_path(sb, path) else {
        return format!("Error: invalid path '{}'", path);
    };

    if let Err(e) = std::fs::create_dir_all(sb) {
        return format!("Error creating sandbox: {}", e);
    };

    if let Ok(count) = count_files_recursive(sb) {
        if count >= MAX_SANDBOX_FILES && !full_path.exists() {
            return format!(
                "Error: sandbox file limit reached (max {})",
                MAX_SANDBOX_FILES
            );
        }
    }

    if let Some(parent) = full_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return format!("Error creating directory: {}", e);
        }
    }

    match std::fs::write(&full_path, content) {
        Ok(()) => "ok".to_string(),
        Err(e) => format!("Error writing '{}': {}", path, e),
    }
}

fn sandbox_file_exists(sandbox: &Option<PathBuf>, path: &str) -> bool {
    let Some(sb) = sandbox else {
        return false;
    };
    resolve_sandbox_path(sb, path)
        .map(|p| p.exists())
        .unwrap_or(false)
}

/// List files in the sandbox directory (top-level only, non-recursive).
fn sandbox_file_list<'js>(ctx: Ctx<'js>, sandbox: &Option<PathBuf>) -> rquickjs::Array<'js> {
    let array = rquickjs::Array::new(ctx.clone()).unwrap();
    let Some(sb) = sandbox else {
        return array;
    };
    if !sb.exists() {
        return array;
    }

    if let Ok(entries) = std::fs::read_dir(sb) {
        let mut i = 0;
        for entry in entries.flatten() {
            if let Ok(rel) = entry.path().strip_prefix(sb) {
                array.set(i, rel.to_string_lossy().to_string()).unwrap();
                i += 1;
            }
        }
    }
    array
}

// ── HTTP Inner Helper ─────────────────────────────────────────

fn stringify_jsvalue<'js>(ctx: Ctx<'js>, v: JsValue<'js>) -> String {
    if let Some(s) = v.as_string() {
        return s.to_string().unwrap_or_default();
    }
    let obj: Object<'js> = ctx.globals().get("JSON").unwrap();
    let stringify: Function<'js> = obj.get("stringify").unwrap();
    stringify.call::<_, String>((v,)).unwrap_or_default()
}

fn http_call_sync<'js>(
    ctx: Ctx<'js>,
    client: &Client,
    method: &str,
    url: &str,
    body: Option<JsValue<'js>>,
    headers_obj: Option<Object<'js>>,
) -> Object<'js> {
    let handle = match Handle::try_current() {
        Ok(h) => h,
        Err(_) => {
            warn!("[js] No tokio runtime — returning error");
            let out = Object::new(ctx.clone()).unwrap();
            out.set("status", 0).unwrap();
            out.set("body", "Error: no async runtime").unwrap();
            return out;
        }
    };

    let body_json = body.map(|v| stringify_jsvalue(ctx.clone(), v));

    let mut parsed_headers = HashMap::new();
    if let Some(hdrs) = headers_obj {
        for prop in hdrs.props::<String, String>() {
            if let Ok((k, v)) = prop {
                parsed_headers.insert(k, v);
            }
        }
    }

    let url_copy = url.to_string();
    let method_copy = method.to_string();
    let caller_set_content_type = has_content_type_header(&parsed_headers);

    let result = tokio::task::block_in_place(|| {
        handle.block_on(async move {
            let mut req = match method_copy.as_str() {
                "POST" => client.post(&url_copy),
                "PUT" => client.put(&url_copy),
                "DELETE" => client.delete(&url_copy),
                _ => client.get(&url_copy),
            };

            if let Some(ref json_str) = body_json {
                if !caller_set_content_type {
                    req = req.header("Content-Type", "application/json");
                }
                req = req.body(json_str.clone());
            }

            for (k, v) in parsed_headers {
                req = req.header(&k, &v);
            }

            match req.send().await {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let body_text = resp.text().await.unwrap_or_default();
                    Ok((status, body_text))
                }
                Err(e) => {
                    warn!("[js] HTTP {} {} failed: {}", method_copy, url_copy, e);
                    Err(format!("Error: {}", e))
                }
            }
        })
    });

    let out = Object::new(ctx).unwrap();
    match result {
        Ok((status, body)) => {
            out.set("status", status).unwrap();
            out.set("body", body).unwrap();
        }
        Err(e) => {
            out.set("status", 0).unwrap();
            out.set("body", e).unwrap();
        }
    }
    out
}

impl Default for QuickJsToolEngine {
    fn default() -> Self {
        Self::new(None, Arc::new(std::sync::RwLock::new(SecretMap::new())))
    }
}

fn has_content_type_header(headers: &HashMap<String, String>) -> bool {
    headers
        .keys()
        .any(|key| key.eq_ignore_ascii_case("Content-Type"))
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::{ParamDef, ToolDef};

    fn make_simple_tool(script: &str, params: Vec<ParamDef>) -> ToolDef {
        ToolDef {
            description: "test tool".to_string(),
            script: script.to_string(),
            params,
            cancel_on_barge_in: true,
            side_effect: false,
            summarize_result: false,
        }
    }

    #[test]
    fn simple_string_return() {
        let mut tools = HashMap::new();
        tools.insert(
            "greet".to_string(),
            make_simple_tool(
                r#"return `Hello, ${name}!`;"#,
                vec![ParamDef {
                    name: "name".to_string(),
                    r#type: "string".to_string(),
                    description: String::new(),
                    required: true,
                    options: vec![],
                }],
            ),
        );
        let engine = QuickJsToolEngine::with_tools(tools);
        let result = engine.execute("greet", r#"{"name": "Alice"}"#).unwrap();
        assert_eq!(result, "Hello, Alice!");
    }

    #[test]
    fn number_arithmetic() {
        let mut tools = HashMap::new();
        tools.insert(
            "add".to_string(),
            make_simple_tool(
                "let sum = a + b; return `The sum is ${sum}`",
                vec![
                    ParamDef {
                        name: "a".to_string(),
                        r#type: "number".to_string(),
                        description: String::new(),
                        required: true,
                        options: vec![],
                    },
                    ParamDef {
                        name: "b".to_string(),
                        r#type: "number".to_string(),
                        description: String::new(),
                        required: true,
                        options: vec![],
                    },
                ],
            ),
        );
        let engine = QuickJsToolEngine::with_tools(tools);
        let result = engine.execute("add", r#"{"a": 3, "b": 4}"#).unwrap();
        assert_eq!(result, "The sum is 7");
    }

    #[test]
    fn empty_args() {
        let mut tools = HashMap::new();
        tools.insert(
            "hello".to_string(),
            make_simple_tool("return 'Hello, World!';", vec![]),
        );
        let engine = QuickJsToolEngine::with_tools(tools);
        let result = engine.execute("hello", "{}").unwrap();
        assert_eq!(result, "Hello, World!");
    }

    #[test]
    fn tool_not_found() {
        let engine = QuickJsToolEngine::with_tools(HashMap::new());
        let result = engine.execute("nonexistent", "{}");
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::NotFound(name) => assert_eq!(name, "nonexistent"),
            other => panic!("Expected NotFound, got {:?}", other),
        }
    }

    #[test]
    fn script_syntax_error() {
        let mut tools = HashMap::new();
        tools.insert(
            "bad".to_string(),
            make_simple_tool("return {{{invalid syntax", vec![]),
        );
        let engine = QuickJsToolEngine::with_tools(tools);
        let result = engine.execute("bad", "{}");
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::ScriptError { name, reason } => {
                assert_eq!(name, "bad");
                assert!(reason.contains("Execution failed"), "reason: {}", reason);
            }
            other => panic!("Expected ScriptError, got {:?}", other),
        }
    }

    #[test]
    fn content_type_detection_is_case_insensitive() {
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        assert!(has_content_type_header(&headers));
    }

    #[test]
    fn file_io_roundtrip() {
        let sandbox = std::env::temp_dir().join(format!("quickjs_test_{}", std::process::id()));
        std::fs::create_dir_all(&sandbox).unwrap();

        let mut tools = HashMap::new();
        tools.insert(
            "write_file".to_string(),
            make_simple_tool(r#"return file_write("test.txt", "hello world");"#, vec![]),
        );
        tools.insert(
            "read_file".to_string(),
            make_simple_tool(r#"return file_read("test.txt");"#, vec![]),
        );
        tools.insert(
            "check_file".to_string(),
            make_simple_tool(r#"return file_exists("test.txt") ? "yes" : "no";"#, vec![]),
        );
        tools.insert(
            "list_files".to_string(),
            make_simple_tool(
                r#"let files = file_list(); return JSON.stringify(files);"#,
                vec![],
            ),
        );

        let engine = QuickJsToolEngine::with_tools_and_sandbox(
            tools,
            sandbox.clone(),
            Arc::new(std::sync::RwLock::new(SecretMap::new())),
        );

        let write_result = engine.execute("write_file", "{}").unwrap();
        assert_eq!(write_result, "ok");

        let read_result = engine.execute("read_file", "{}").unwrap();
        assert_eq!(read_result, "hello world");

        let exists_result = engine.execute("check_file", "{}").unwrap();
        assert_eq!(exists_result, "yes");

        let list_result = engine.execute("list_files", "{}").unwrap();
        assert!(
            list_result.contains("test.txt"),
            "list_result: {}",
            list_result
        );

        // Cleanup
        let _ = std::fs::remove_dir_all(&sandbox);
    }

    #[test]
    fn secret_found_and_missing() {
        let mut secrets = SecretMap::new();
        secrets.insert("api_key".to_string(), "sk-123".into());

        let mut tools = HashMap::new();
        tools.insert(
            "get_secret".to_string(),
            make_simple_tool(
                r#"return secret(key);"#,
                vec![ParamDef {
                    name: "key".to_string(),
                    r#type: "string".to_string(),
                    description: String::new(),
                    required: true,
                    options: vec![],
                }],
            ),
        );

        let engine = QuickJsToolEngine::with_tools_and_sandbox(
            tools,
            std::env::temp_dir(),
            Arc::new(std::sync::RwLock::new(secrets)),
        );

        // Found
        let result = engine
            .execute("get_secret", r#"{"key": "api_key"}"#)
            .unwrap();
        assert_eq!(result, "sk-123");

        // Missing — should return empty string, not error message (prevents key name leak)
        let result = engine
            .execute("get_secret", r#"{"key": "missing"}"#)
            .unwrap();
        assert!(
            result.is_empty(),
            "Expected empty string on missing secret, got: '{}'",
            result
        );
    }

    #[test]
    fn json_object_return() {
        let mut tools = HashMap::new();
        tools.insert(
            "obj".to_string(),
            make_simple_tool(r#"return {status: 200, message: "ok"};"#, vec![]),
        );
        let engine = QuickJsToolEngine::with_tools(tools);
        let result = engine.execute("obj", "{}").unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], 200);
        assert_eq!(parsed["message"], "ok");
    }

    /// Compile-time assertion that `QuickJsToolEngine` is `Send + Sync`.
    /// Required because it is wrapped in `Arc` and shared across tasks.
    #[test]
    fn engine_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<QuickJsToolEngine>();
    }

    #[test]
    fn infinite_loop_timeout() {
        let mut tools = HashMap::new();
        tools.insert(
            "loop".to_string(),
            make_simple_tool("while(true) {} return 'done';", vec![]),
        );
        let mut engine = QuickJsToolEngine::with_tools(tools);
        // Use a very short timeout so the test doesn't take 60s
        engine.script_timeout = Duration::from_millis(100);

        let start = Instant::now();
        let result = engine.execute("loop", "{}");
        let elapsed = start.elapsed();

        assert!(result.is_err(), "Expected timeout error, got: {:?}", result);
        // Should complete well under 5 seconds (the interrupt handler fires quickly)
        assert!(
            elapsed < Duration::from_secs(5),
            "Took too long: {:?}",
            elapsed
        );
    }
}
