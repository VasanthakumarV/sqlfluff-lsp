use std::collections::HashMap;

use tokio::sync::{RwLock, watch};
use tower_lsp_server::{Client, LanguageServer, jsonrpc::Result, lsp_types::*};
use tracing::error;

use crate::sqlfluff;

#[derive(Debug)]
pub struct Backend {
    client: Client,
    config: Config,
    watchers: RwLock<HashMap<Uri, Watcher>>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub dialect: Option<String>,
    pub templater: Option<String>,
    pub sqlfluff_path: Option<String>,
}

#[derive(Debug)]
struct Watcher {
    tx: watch::Sender<String>,
    rx: watch::Receiver<String>,
}

impl Backend {
    pub fn new(
        client: Client,
        dialect: Option<String>,
        templater: Option<String>,
        sqlfluff_path: Option<String>,
    ) -> Self {
        Self {
            client,
            config: Config {
                dialect,
                templater,
                sqlfluff_path,
            },
            watchers: RwLock::new(HashMap::new()),
        }
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            server_info: None,
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                document_formatting_provider: Some(OneOf::Right(DocumentFormattingOptions {
                    work_done_progress_options: WorkDoneProgressOptions {
                        work_done_progress: Some(false),
                    },
                })),
                ..ServerCapabilities::default()
            },
        })
    }

    async fn initialized(&self, _: InitializedParams) {}

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn formatting(
        &self,
        DocumentFormattingParams {
            text_document: TextDocumentIdentifier { uri },
            ..
        }: DocumentFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        let config = self.config.clone();
        if let Some(content) = self
            .watchers
            .read()
            .await
            .get(&uri)
            .map(|bar| bar.rx.borrow().clone())
        {
            let output = match sqlfluff::fmt(&uri, &content, config).await {
                Ok(output) => output,
                Err(error) => {
                    error!("{error}");
                    self.client.show_message(MessageType::ERROR, error).await;
                    return Ok(None);
                }
            };

            Ok(Some(output))
        } else {
            Ok(None)
        }
    }

    async fn did_open(
        &self,
        DidOpenTextDocumentParams {
            text_document: TextDocumentItem { uri, text, .. },
        }: DidOpenTextDocumentParams,
    ) {
        let config = self.config.clone();
        self.watchers
            .write()
            .await
            .entry(uri.clone())
            .and_modify(|watcher| watcher.tx.send(text.clone()).unwrap())
            .or_insert_with(|| {
                let (tx, rx) = watch::channel(text);

                let client = self.client.clone();
                let mut _rx = rx.clone();
                tokio::spawn(async move {
                    loop {
                        let content = _rx.borrow_and_update().clone();

                        match sqlfluff::lint(&uri, &content, config.clone()).await {
                            Ok(diags) => {
                                client.publish_diagnostics(uri.clone(), diags, None).await;
                            }
                            Err(error) => {
                                error!("{error}");
                                client.show_message(MessageType::ERROR, error).await;
                            }
                        }

                        if _rx.changed().await.is_err() {
                            break;
                        }
                    }
                });

                Watcher { tx, rx }
            });
    }

    async fn did_close(
        &self,
        DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier { uri },
        }: DidCloseTextDocumentParams,
    ) {
        self.watchers.write().await.remove(&uri);
        self.client.publish_diagnostics(uri, vec![], None).await;
    }

    async fn did_change(
        &self,
        DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier { uri, .. },
            content_changes,
        }: DidChangeTextDocumentParams,
    ) {
        if let Some(change) = content_changes.first()
            && let Some(watcher) = self.watchers.read().await.get(&uri)
        {
            watcher.tx.send(change.text.clone()).unwrap();
        }
    }

    async fn did_save(
        &self,
        DidSaveTextDocumentParams {
            text_document: TextDocumentIdentifier { uri },
            text,
        }: DidSaveTextDocumentParams,
    ) {
        if let Some(text) = text
            && let Some(watcher) = self.watchers.read().await.get(&uri)
        {
            watcher.tx.send(text).unwrap();
        }
    }
}
