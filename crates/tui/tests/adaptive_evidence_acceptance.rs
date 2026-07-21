//! Process-level acceptance for adaptive exact-evidence routing (#4619).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use wait_timeout::ChildExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const MODEL: &str = "adaptive-evidence-test";
const CALL_ID: &str = "call_evidence";
const DEEP_SENTINEL: &str = "DEEP_EXACT_EVIDENCE_SENTINEL_4619";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn headless_tool_output_is_bounded_while_exact_origin_artifact_is_retained() {
    let workspace = TempDir::new().expect("workspace");
    let home = TempDir::new().expect("home");
    let mut source = String::new();
    for line in 0..500 {
        let marker = if line == 80 {
            DEEP_SENTINEL
        } else {
            "ordinary"
        };
        source.push_str(&format!("{line:04} {marker} {}\n", "x".repeat(96)));
    }
    std::fs::write(workspace.path().join("large.txt"), source).expect("large fixture");

    let server = mock_llm().await;
    let output = run_exec(workspace.path(), home.path(), &server);
    assert!(
        output.status.success(),
        "exec failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let requests = server.received_requests().await.expect("recorded requests");
    let tool_content = requests
        .iter()
        .filter_map(|request| request.body_json::<Value>().ok())
        .find_map(|body| tool_result_content(&body).map(str::to_owned))
        .expect("model-visible tool result");
    assert!(tool_content.starts_with("[Exact evidence retained"));
    assert!(tool_content.contains("retrieve_tool_result ref=art_call_evidence"));
    assert!(!tool_content.contains(DEEP_SENTINEL));
    assert!(!tool_content.contains("/artifacts/"));
    assert!(
        tool_content.len() <= 3_200,
        "handle-only receipt must stay bounded"
    );

    let artifact_dir = find_artifact_dir(home.path()).expect("origin-session artifacts");
    let payloads: Vec<_> = std::fs::read_dir(&artifact_dir)
        .expect("artifact directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("txt"))
        .collect();
    assert_eq!(payloads.len(), 1, "exactly one evidence payload per result");
    assert_eq!(
        payloads[0].file_name().and_then(|name| name.to_str()),
        Some("art_call_evidence.txt")
    );
    let exact = std::fs::read(&payloads[0]).expect("exact evidence bytes");
    assert!(
        String::from_utf8_lossy(&exact).contains(DEEP_SENTINEL),
        "deep content omitted from context must remain retrievable"
    );

    let metadata_path = artifact_dir.join("art_call_evidence.evidence.json");
    let metadata: Value =
        serde_json::from_slice(&std::fs::read(&metadata_path).expect("evidence metadata"))
            .expect("valid evidence metadata");
    let digest = Sha256::digest(&exact)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(metadata["handle"], "art_call_evidence");
    assert_eq!(metadata["call_id"], CALL_ID);
    assert_eq!(metadata["tool_name"], "read_file");
    assert_eq!(metadata["digest"], digest);
    assert_eq!(metadata["size_bytes"], exact.len() as u64);
    assert_eq!(metadata["generation"], 1);
    assert_eq!(metadata["redacted"], false);
    assert_eq!(metadata["encoding"], "utf-8");
    assert_eq!(metadata["retention_state"], "live");
    assert!(
        metadata["origin_session"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
}

async fn mock_llm() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(json_response(json!({
            "object": "list",
            "data": [{"id": MODEL, "object": "model"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(has_tool_result)
        .respond_with(sse_response(final_sse()))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(has_no_tool_result)
        .respond_with(sse_response(tool_sse()))
        .mount(&server)
        .await;
    server
}

fn run_exec(workspace: &Path, home: &Path, server: &MockServer) -> std::process::Output {
    std::fs::create_dir_all(home.join(".codewhale")).expect("config directory");
    std::fs::create_dir_all(home.join(".deepseek")).expect("legacy config directory");
    let mut command = Command::new(binary());
    preserve_host_env(&mut command);
    command
        .current_dir(workspace)
        .args(["--workspace", workspace.to_str().expect("workspace utf8")])
        .arg("--no-project-config")
        .args([
            "exec",
            "--auto",
            "--model",
            MODEL,
            "--output-format",
            "stream-json",
        ])
        .arg("read the complete large.txt file")
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("CODEWHALE_CONFIG_PATH", home.join(".codewhale/config.toml"))
        .env("DEEPSEEK_CONFIG_PATH", home.join(".deepseek/config.toml"))
        .env("DEEPSEEK_API_KEY", "ci-test-key-not-real")
        .env("DEEPSEEK_BASE_URL", server.uri())
        .env("CODEWHALE_BASE_URL", server.uri())
        .env("DEEPSEEK_MODEL", MODEL)
        .env("CODEWHALE_MODEL", MODEL)
        .env("RUST_LOG", "warn")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    run_with_timeout(command, Duration::from_secs(45))
}

fn find_artifact_dir(home: &Path) -> Option<PathBuf> {
    let sessions = home.join(".codewhale/sessions");
    std::fs::read_dir(sessions)
        .ok()?
        .filter_map(Result::ok)
        .find_map(|entry| {
            let path = entry.path().join("artifacts");
            path.is_dir().then_some(path)
        })
}

fn tool_result_content(body: &Value) -> Option<&str> {
    body.get("messages")?
        .as_array()?
        .iter()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("tool"))?
        .get("content")?
        .as_str()
}

fn has_tool_result(request: &Request) -> bool {
    request
        .body_json::<Value>()
        .ok()
        .and_then(|body| tool_result_content(&body).map(str::to_owned))
        .is_some()
}

fn has_no_tool_result(request: &Request) -> bool {
    !has_tool_result(request)
}

fn tool_sse() -> String {
    let arguments = serde_json::to_string(&json!({"path": "large.txt", "max_lines": 500}))
        .expect("tool arguments");
    [
        chunk(json!({"id":"tool","object":"chat.completion.chunk","model":MODEL,"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":CALL_ID,"type":"function","function":{"name":"read_file","arguments":arguments}}]},"finish_reason":null}]})),
        chunk(json!({"id":"tool","object":"chat.completion.chunk","model":MODEL,"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}})),
        "data: [DONE]\n\n".to_string(),
    ].join("")
}

fn final_sse() -> String {
    [
        chunk(json!({"id":"final","object":"chat.completion.chunk","model":MODEL,"choices":[{"index":0,"delta":{"content":"evidence retained"},"finish_reason":null}]})),
        chunk(json!({"id":"final","object":"chat.completion.chunk","model":MODEL,"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":20,"completion_tokens":2,"total_tokens":22}})),
        "data: [DONE]\n\n".to_string(),
    ].join("")
}

fn chunk(value: Value) -> String {
    format!(
        "data: {}\n\n",
        serde_json::to_string(&value).expect("SSE JSON")
    )
}

fn sse_response(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

fn json_response(value: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(value)
}

fn binary() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_codewhale-tui")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/codewhale-tui")
        })
}

fn preserve_host_env(command: &mut Command) {
    command.env_clear();
    for key in [
        "PATH",
        "PATHEXT",
        "SystemRoot",
        "SystemDrive",
        "WINDIR",
        "COMSPEC",
        "TEMP",
        "TMP",
        "TERM",
        "LANG",
        "LC_ALL",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
}

fn run_with_timeout(mut command: Command, timeout: Duration) -> std::process::Output {
    let mut child = command.spawn().expect("spawn codewhale exec");
    let stdout = read_in_background(child.stdout.take().expect("stdout"));
    let stderr = read_in_background(child.stderr.take().expect("stderr"));
    let status = child
        .wait_timeout(timeout)
        .expect("wait")
        .unwrap_or_else(|| {
            let _ = child.kill();
            let _ = child.wait();
            panic!("codewhale exec timed out")
        });
    std::process::Output {
        status,
        stdout: stdout.join().expect("stdout thread").expect("read stdout"),
        stderr: stderr.join().expect("stderr thread").expect("read stderr"),
    }
}

fn read_in_background<R: Read + Send + 'static>(
    mut reader: R,
) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).map(|_| bytes)
    })
}
