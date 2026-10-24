use super::*;
use crate::command::{CommandContext, CommandHandler};
use crate::elicitation::ElicitationRequest;
use crate::output::CommandResult;
use rmcp::ServiceExt;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct Ask {
    url: bool,
}

#[async_trait::async_trait]
impl CommandHandler for Ask {
    async fn run(&self, context: CommandContext) -> CommandResult {
        let channel = context.request.and_then(|request| request.elicitation);
        let request = if self.url {
            ElicitationRequest::Url {
                message: "Connect your account".into(),
                url: "https://example.com/connect".into(),
                elicitation_id: "literal-connection-id".into(),
            }
        } else {
            ElicitationRequest::Form {
                message: "Choose a city".into(),
                requested_schema: json!({"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}),
            }
        };
        let response = match channel {
            Some(channel) => channel.request(request).await,
            None => Err("No elicitation channel".into()),
        };
        CommandResult::Ok {
            data: json!({"response":response}),
            cta: None,
            exit_code: None,
        }
    }
}

type Reader = BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>;
type Writer = tokio::io::WriteHalf<tokio::io::DuplexStream>;
struct Client {
    reader: Reader,
    writer: Writer,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Client {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Client {
    async fn write(&mut self, value: Value) {
        self.writer
            .write_all(format!("{value}\n").as_bytes())
            .await
            .unwrap();
    }
    async fn read(&mut self) -> Value {
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            self.reader.read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        serde_json::from_str(&line).unwrap()
    }
}
async fn connect(url: bool, supported: bool) -> Client {
    let cli = crate::cli::Cli::create("ask").command(
        "city",
        crate::command::CommandDef::build("city", Ask { url }).done(),
    );
    let options = McpServeOptions {
        tools: super::super::McpToolFilter {
            discovery: super::super::McpDiscovery::Direct,
            ..Default::default()
        },
        ..Default::default()
    };
    let handler = IncurMcpServer::new(&ServerSource::from_cli(&cli), &options).unwrap();
    let (transport, client) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        handler
            .serve(transport)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap();
    });
    let (reader, writer) = tokio::io::split(client);
    let mut client = Client {
        reader: BufReader::new(reader),
        writer,
        server,
    };
    let mut init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{"elicitation":{"form":{}}},"clientInfo":{"name":"literal","version":"1"}}});
    if url && supported {
        init["params"]["capabilities"]["elicitation"]["url"] = json!({});
    }
    client.write(init).await;
    let initialized = client.read().await;
    assert!(initialized.get("result").is_some(), "{initialized}");
    client
        .write(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .await;
    client.write(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"city","arguments":{}}})).await;
    client
}
fn data(response: &Value) -> Value {
    serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}
#[tokio::test]
async fn command_receives_each_explicit_elicitation_action() {
    for (action, url, supported) in [
        ("accept", false, true),
        ("decline", false, true),
        ("cancel", false, true),
        ("accept", true, true),
        ("accept", true, false),
    ] {
        let mut client = connect(url, supported).await;
        let request = client.read().await;
        if !supported {
            assert_eq!(
                data(&request)["response"]["Err"],
                "Client does not support this elicitation mode"
            );
            continue;
        }
        assert_eq!(request["method"], "elicitation/create", "{request}");
        if url {
            assert_eq!(request["params"]["mode"], "url");
            assert_eq!(request["params"]["url"], "https://example.com/connect");
            assert_eq!(request["params"]["elicitationId"], "literal-connection-id");
        } else {
            assert_eq!(
                request["params"]["requestedSchema"]["required"],
                json!(["city"])
            );
        }
        let mut result = json!({"action":action});
        if action == "accept" && !url {
            result["content"] = json!({"city":"Detroit"});
        }
        client
            .write(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
            .await;
        let response = client.read().await;
        assert_eq!(
            data(&response)["response"]["Ok"]["action"],
            action,
            "{response}"
        );
    }
}
