use std::{path::PathBuf, pin::Pin, sync::Arc, time::SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Duration, Utc};
use clap::Parser;
use mongodb::{
    Client as MongoClient,
    bson::{DateTime as BsonDateTime, doc},
};
use rcgen::generate_simple_self_signed;
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    time,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{ServerConfig, pki_types::CertificateDer, pki_types::PrivateKeyDer},
};

trait SmtpIo: AsyncBufRead + AsyncWrite {}

impl<T> SmtpIo for T where T: AsyncBufRead + AsyncWrite {}

type SmtpStream = Pin<Box<dyn SmtpIo + Send>>;

#[derive(Parser, Debug, Clone)]
#[command(author, version, about = "Simple insecure SMTP receiver for testing")]
struct Args {
    /// Address to bind. Use 0.0.0.0:25 to listen on port 25 for all interfaces.
    #[arg(long, default_value = "0.0.0.0:25", env = "MAIL_BOX_BIND")]
    bind: String,

    /// Save the full SMTP conversation to a text file.
    #[arg(long, default_value_t = true, env = "MAIL_BOX_SAVE_TRANSCRIPT")]
    save_transcript: bool,

    /// Disable saving the full SMTP conversation to a text file.
    #[arg(
        long,
        conflicts_with = "save_transcript",
        env = "MAIL_BOX_NO_SAVE_TRANSCRIPT"
    )]
    no_save_transcript: bool,

    /// Directory for transcript files.
    #[arg(
        long,
        default_value = "save-transcript",
        env = "MAIL_BOX_TRANSCRIPT_DIR"
    )]
    transcript_dir: PathBuf,

    /// JSON file containing webhook config: {"url":"https://example.com/hook"}.
    #[arg(long, env = "MAIL_BOX_WEBHOOK_CONFIG")]
    webhook_config: Option<PathBuf>,

    /// Print SMTP conversation and processing logs to stdout.
    #[arg(long, env = "MAIL_BOX_DEBUG")]
    debug: bool,

    /// Firebase Realtime Database base URL, for example https://project.firebaseio.com.
    #[arg(long, env = "MAIL_BOX_FIREBASE_URL")]
    firebase_url: Option<String>,

    /// Firebase auth token if your database rules require it.
    #[arg(long, env = "MAIL_BOX_FIREBASE_AUTH")]
    firebase_auth: Option<String>,

    /// Firebase collection/path used to push messages.
    #[arg(long, default_value = "emails", env = "MAIL_BOX_FIREBASE_PATH")]
    firebase_path: String,

    /// MongoDB connection string.
    #[arg(long, env = "MAIL_BOX_MONGODB_URI")]
    mongodb_uri: Option<String>,

    /// MongoDB database name.
    #[arg(long, default_value = "mail_box", env = "MAIL_BOX_MONGODB_DATABASE")]
    mongodb_database: String,

    /// MongoDB collection name.
    #[arg(long, default_value = "emails", env = "MAIL_BOX_MONGODB_COLLECTION")]
    mongodb_collection: String,

    /// Disable automatic cleanup of old received data.
    #[arg(long, env = "MAIL_BOX_NO_CLEANUP")]
    no_cleanup: bool,

    /// Cleanup interval in minutes.
    #[arg(long, default_value_t = 5, env = "MAIL_BOX_CLEANUP_INTERVAL_MINUTES")]
    cleanup_interval_minutes: u64,

    /// Delete received data older than this many minutes.
    #[arg(long, default_value_t = 30, env = "MAIL_BOX_CLEANUP_RETENTION_MINUTES")]
    cleanup_retention_minutes: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct WebhookConfig {
    url: String,
}

#[derive(Clone)]
struct AppState {
    args: Args,
    http: reqwest::Client,
    tls_acceptor: TlsAcceptor,
    webhook: Option<WebhookConfig>,
    mongo: Option<MongoTarget>,
}

#[derive(Clone)]
struct MongoTarget {
    client: MongoClient,
    database: String,
    collection: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ReceivedEmail {
    peer_addr: String,
    from: String,
    recipients: Vec<String>,
    data: String,
    transcript: String,
    received_at: DateTime<Utc>,
}

#[tokio::main]
async fn main() -> Result<()> {
    install_crypto_provider();

    let args = Args::parse();
    let webhook = load_webhook_config(args.webhook_config.as_ref()).await?;
    let mongo = load_mongo_target(&args).await?;
    let listener = TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("failed to bind {}", args.bind))?;

    let state = Arc::new(AppState {
        args,
        http: reqwest::Client::new(),
        tls_acceptor: build_tls_acceptor()?,
        webhook,
        mongo,
    });

    log_debug(
        &state.args,
        format_args!("listening on {}", listener.local_addr()?),
    );

    if !state.args.no_cleanup {
        tokio::spawn(run_cleanup_loop(Arc::clone(&state)));
    }

    loop {
        let (stream, peer_addr) = listener.accept().await?;
        let state = Arc::clone(&state);

        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, peer_addr.to_string(), state).await {
                eprintln!("connection error from {peer_addr}: {error:#}");
            }
        });
    }
}

fn install_crypto_provider() {
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
}

async fn load_webhook_config(path: Option<&PathBuf>) -> Result<Option<WebhookConfig>> {
    let Some(path) = path else {
        return Ok(None);
    };

    let content = fs::read_to_string(path)
        .await
        .with_context(|| format!("failed to read webhook config {}", path.display()))?;
    let config: WebhookConfig = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse webhook config {}", path.display()))?;

    if config.url.trim().is_empty() {
        bail!("webhook config url cannot be empty");
    }

    Ok(Some(config))
}

async fn load_mongo_target(args: &Args) -> Result<Option<MongoTarget>> {
    let Some(uri) = args.mongodb_uri.as_ref() else {
        return Ok(None);
    };

    let client = MongoClient::with_uri_str(uri)
        .await
        .context("failed to connect to MongoDB")?;

    Ok(Some(MongoTarget {
        client,
        database: args.mongodb_database.clone(),
        collection: args.mongodb_collection.clone(),
    }))
}

async fn handle_connection(
    stream: TcpStream,
    peer_addr: String,
    state: Arc<AppState>,
) -> Result<()> {
    let mut session = SmtpSession::new(peer_addr);
    let mut stream: SmtpStream = Box::pin(BufReader::new(stream));
    let mut tls_enabled = false;

    write_response(
        &state.args,
        &mut session,
        &mut stream,
        "220 mail-box ready\r\n",
    )
    .await?;

    loop {
        let mut line = String::new();
        let bytes_read = stream.read_line(&mut line).await?;
        if bytes_read == 0 {
            return Ok(());
        }

        session.record_client(&state.args, &line);
        let command = line.trim_end_matches(['\r', '\n']);
        let upper = command.to_ascii_uppercase();

        if upper.starts_with("HELO ") || upper.starts_with("EHLO ") {
            let response = if tls_enabled {
                "250-mail-box\r\n250 OK\r\n"
            } else {
                "250-mail-box\r\n250-STARTTLS\r\n250 OK\r\n"
            };
            write_response(&state.args, &mut session, &mut stream, response).await?;
        } else if upper.starts_with("MAIL FROM:") {
            session.from = parse_address(command, "MAIL FROM:");
            write_response(&state.args, &mut session, &mut stream, "250 sender ok\r\n").await?;
        } else if upper.starts_with("RCPT TO:") {
            session.recipients.push(parse_address(command, "RCPT TO:"));
            write_response(
                &state.args,
                &mut session,
                &mut stream,
                "250 recipient ok\r\n",
            )
            .await?;
        } else if upper == "DATA" {
            write_response(
                &state.args,
                &mut session,
                &mut stream,
                "354 end with <CRLF>.<CRLF>\r\n",
            )
            .await?;
            read_message_data(&mut stream, &mut session, &state.args).await?;
            write_response(
                &state.args,
                &mut session,
                &mut stream,
                "250 message accepted\r\n",
            )
            .await?;

            let email = session.to_email();
            process_email(email, &state).await?;
            session.reset_message();
        } else if upper == "RSET" {
            session.reset_message();
            write_response(&state.args, &mut session, &mut stream, "250 reset ok\r\n").await?;
        } else if upper == "NOOP" {
            write_response(&state.args, &mut session, &mut stream, "250 ok\r\n").await?;
        } else if upper == "STARTTLS" {
            if tls_enabled {
                write_response(
                    &state.args,
                    &mut session,
                    &mut stream,
                    "250 already using TLS\r\n",
                )
                .await?;
            } else {
                write_response(
                    &state.args,
                    &mut session,
                    &mut stream,
                    "220 ready to start TLS\r\n",
                )
                .await?;
                stream = Box::pin(BufReader::new(
                    state
                        .tls_acceptor
                        .accept(stream)
                        .await
                        .context("TLS handshake failed")?,
                ));
                tls_enabled = true;
                session.record_server("TLS: handshake completed\r\n");
                log_debug(&state.args, format_args!("TLS handshake completed"));
            }
        } else if upper == "QUIT" {
            write_response(&state.args, &mut session, &mut stream, "221 bye\r\n").await?;
            return Ok(());
        } else {
            write_response(&state.args, &mut session, &mut stream, "250 ok\r\n").await?;
        }
    }
}

fn build_tls_acceptor() -> Result<TlsAcceptor> {
    let cert = generate_simple_self_signed(vec!["localhost".to_string()])?;
    let cert_der = CertificateDer::from(cert.cert.der().to_vec());
    let key_der = PrivateKeyDer::try_from(cert.key_pair.serialize_der())
        .map_err(|error| anyhow!("failed to create TLS private key: {error}"))?;
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .context("failed to create TLS config")?;

    Ok(TlsAcceptor::from(Arc::new(config)))
}

async fn read_message_data<R>(reader: &mut R, session: &mut SmtpSession, args: &Args) -> Result<()>
where
    R: AsyncBufRead + Unpin,
{
    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            bail!("connection closed while reading DATA");
        }

        session.record_client(args, &line);
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "." {
            return Ok(());
        }

        let data_line = trimmed.strip_prefix("..").unwrap_or(trimmed);
        session.data.push_str(data_line);
        session.data.push('\n');
    }
}

async fn write_response<W>(
    args: &Args,
    session: &mut SmtpSession,
    writer: &mut W,
    response: &str,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    session.record_server(response);
    writer.write_all(response.as_bytes()).await?;
    writer.flush().await?;
    log_debug(args, format_args!("S: {}", response.trim_end()));
    Ok(())
}

async fn process_email(email: ReceivedEmail, state: &AppState) -> Result<()> {
    if state.args.save_transcript && !state.args.no_save_transcript {
        save_transcript(&email, &state.args).await?;
    }

    if let Some(webhook) = state.webhook.as_ref() {
        call_webhook(&email, webhook, state).await?;
    }

    if state.args.firebase_url.is_some() {
        save_firebase(&email, state).await?;
    }

    if let Some(mongo) = state.mongo.as_ref() {
        save_mongodb(&email, mongo).await?;
    }

    Ok(())
}

async fn save_transcript(email: &ReceivedEmail, args: &Args) -> Result<()> {
    fs::create_dir_all(&args.transcript_dir).await?;
    let first_recipient = email
        .recipients
        .first()
        .map(String::as_str)
        .unwrap_or("unknown-recipient");
    let file_name = format!(
        "{}-{}-{}.txt",
        sanitize_file_part(&email.from),
        sanitize_file_part(first_recipient),
        sanitize_file_part(&email.received_at.to_rfc3339())
    );
    let path = args.transcript_dir.join(file_name);

    fs::write(&path, &email.transcript)
        .await
        .with_context(|| format!("failed to write transcript {}", path.display()))?;
    log_debug(args, format_args!("saved transcript {}", path.display()));
    Ok(())
}

async fn call_webhook(
    email: &ReceivedEmail,
    webhook: &WebhookConfig,
    state: &AppState,
) -> Result<()> {
    state
        .http
        .post(&webhook.url)
        .json(email)
        .send()
        .await
        .context("failed to call webhook")?
        .error_for_status()
        .context("webhook returned error status")?;
    log_debug(&state.args, format_args!("called webhook {}", webhook.url));
    Ok(())
}

async fn save_firebase(email: &ReceivedEmail, state: &AppState) -> Result<()> {
    let args = &state.args;
    let base_url = args.firebase_url.as_deref().expect("checked by caller");
    let mut url = format!(
        "{}/{}.json",
        base_url.trim_end_matches('/'),
        args.firebase_path.trim_matches('/')
    );

    if let Some(auth) = args.firebase_auth.as_ref() {
        url.push_str("?auth=");
        url.push_str(auth);
    }

    state
        .http
        .post(&url)
        .json(email)
        .send()
        .await
        .context("failed to save to Firebase")?
        .error_for_status()
        .context("Firebase returned error status")?;
    log_debug(
        args,
        format_args!("saved to Firebase path {}", args.firebase_path),
    );
    Ok(())
}

async fn save_mongodb(email: &ReceivedEmail, mongo: &MongoTarget) -> Result<()> {
    let collection = mongo
        .client
        .database(&mongo.database)
        .collection::<ReceivedEmail>(&mongo.collection);
    collection
        .insert_one(email)
        .await
        .context("failed to save to MongoDB")?;
    Ok(())
}

async fn run_cleanup_loop(state: Arc<AppState>) {
    if let Err(error) = cleanup_old_data(&state).await {
        eprintln!("cleanup error: {error:#}");
    }

    let interval_minutes = state.args.cleanup_interval_minutes.max(1);
    let mut interval = time::interval(time::Duration::from_secs(interval_minutes * 60));

    loop {
        interval.tick().await;

        if let Err(error) = cleanup_old_data(&state).await {
            eprintln!("cleanup error: {error:#}");
        }
    }
}

async fn cleanup_old_data(state: &AppState) -> Result<()> {
    let retention_minutes = state.args.cleanup_retention_minutes.max(1);
    let cutoff = Utc::now() - Duration::minutes(retention_minutes);

    log_debug(
        &state.args,
        format_args!("cleanup started for data older than {cutoff}"),
    );

    if state.args.save_transcript && !state.args.no_save_transcript {
        cleanup_transcripts(&state.args, cutoff).await?;
    }

    if state.args.firebase_url.is_some() {
        cleanup_firebase(state, cutoff).await?;
    }

    if let Some(mongo) = state.mongo.as_ref() {
        cleanup_mongodb(mongo, cutoff, &state.args).await?;
    }

    Ok(())
}

async fn cleanup_transcripts(args: &Args, cutoff: DateTime<Utc>) -> Result<()> {
    let mut entries = match fs::read_dir(&args.transcript_dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("failed to read transcript directory"),
    };

    let mut deleted = 0_u64;

    while let Some(entry) = entries.next_entry().await? {
        let metadata = entry.metadata().await?;

        if !metadata.is_file() {
            continue;
        }

        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let modified_at = DateTime::<Utc>::from(modified);

        if modified_at < cutoff {
            fs::remove_file(entry.path()).await?;
            deleted += 1;
        }
    }

    log_debug(
        args,
        format_args!("cleanup deleted {deleted} transcript file(s)"),
    );
    Ok(())
}

async fn cleanup_firebase(state: &AppState, cutoff: DateTime<Utc>) -> Result<()> {
    let args = &state.args;
    let base_url = args.firebase_url.as_deref().expect("checked by caller");
    let base_path = format!(
        "{}/{}.json",
        base_url.trim_end_matches('/'),
        args.firebase_path.trim_matches('/')
    );
    let mut url = format!(
        "{base_path}?orderBy=\"received_at\"&endAt=\"{}\"",
        cutoff.to_rfc3339()
    );

    if let Some(auth) = args.firebase_auth.as_ref() {
        url.push_str("&auth=");
        url.push_str(auth);
    }

    let old_items = state
        .http
        .get(&url)
        .send()
        .await
        .context("failed to query Firebase cleanup data")?
        .error_for_status()
        .context("Firebase cleanup query returned error status")?
        .json::<serde_json::Value>()
        .await
        .context("failed to parse Firebase cleanup response")?;

    let Some(items) = old_items.as_object() else {
        return Ok(());
    };

    let mut deleted = 0_u64;

    for key in items.keys() {
        let mut delete_url = format!(
            "{}/{}/{}.json",
            base_url.trim_end_matches('/'),
            args.firebase_path.trim_matches('/'),
            key
        );

        if let Some(auth) = args.firebase_auth.as_ref() {
            delete_url.push_str("?auth=");
            delete_url.push_str(auth);
        }

        state
            .http
            .delete(&delete_url)
            .send()
            .await
            .context("failed to delete Firebase cleanup item")?
            .error_for_status()
            .context("Firebase cleanup delete returned error status")?;
        deleted += 1;
    }

    log_debug(
        args,
        format_args!("cleanup deleted {deleted} Firebase item(s)"),
    );
    Ok(())
}

async fn cleanup_mongodb(mongo: &MongoTarget, cutoff: DateTime<Utc>, args: &Args) -> Result<()> {
    let collection = mongo
        .client
        .database(&mongo.database)
        .collection::<ReceivedEmail>(&mongo.collection);
    let bson_cutoff = BsonDateTime::from_millis(cutoff.timestamp_millis());
    let result = collection
        .delete_many(doc! { "received_at": { "$lt": bson_cutoff } })
        .await
        .context("failed to cleanup MongoDB")?;

    log_debug(
        args,
        format_args!("cleanup deleted {} MongoDB item(s)", result.deleted_count),
    );
    Ok(())
}

struct SmtpSession {
    peer_addr: String,
    from: String,
    recipients: Vec<String>,
    data: String,
    transcript: String,
    received_at: DateTime<Utc>,
}

impl SmtpSession {
    fn new(peer_addr: String) -> Self {
        Self {
            peer_addr,
            from: "unknown-sender".to_string(),
            recipients: Vec::new(),
            data: String::new(),
            transcript: String::new(),
            received_at: Utc::now(),
        }
    }

    fn record_client(&mut self, args: &Args, line: &str) {
        self.transcript.push_str("C: ");
        self.transcript.push_str(line);
        log_debug(args, format_args!("C: {}", line.trim_end()));
    }

    fn record_server(&mut self, line: &str) {
        self.transcript.push_str("S: ");
        self.transcript.push_str(line);
    }

    fn reset_message(&mut self) {
        self.from = "unknown-sender".to_string();
        self.recipients.clear();
        self.data.clear();
        self.received_at = Utc::now();
    }

    fn to_email(&self) -> ReceivedEmail {
        ReceivedEmail {
            peer_addr: self.peer_addr.clone(),
            from: self.from.clone(),
            recipients: self.recipients.clone(),
            data: self.data.clone(),
            transcript: self.transcript.clone(),
            received_at: self.received_at,
        }
    }
}

fn parse_address(command: &str, prefix: &str) -> String {
    command[prefix.len()..]
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_string()
}

fn sanitize_file_part(input: &str) -> String {
    let sanitized: String = input
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect();

    sanitized.trim_matches('-').to_string()
}

fn log_debug(args: &Args, message: std::fmt::Arguments<'_>) {
    if args.debug {
        println!("{message}");
    }
}
