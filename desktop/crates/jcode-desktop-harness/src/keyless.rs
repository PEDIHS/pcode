//! Account-based and local model connectors, separated from API-key sign-in.
//! The helper is embedded in the binary and runs only on the loopback device.
use std::path::{Path, PathBuf};
use std::time::Duration;

const PORT: u16 = 42187;
const VERSION: &str = "1.0.0";
const SERVER: &str = include_str!("keyless/server.py");
const PAGE: &str = include_str!("keyless/index.html");

fn home() -> Result<PathBuf, String> {
    std::env::var_os("JCODE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(|path| PathBuf::from(path).join(".jcode"))
        })
        .ok_or_else(|| "Could not locate the Jcode account directory.".into())
}

fn healthy(client: &reqwest::blocking::Client) -> bool {
    client
        .get(format!("http://127.0.0.1:{PORT}/health"))
        .send()
        .and_then(|r| r.json::<serde_json::Value>())
        .is_ok_and(|v| v["service"] == "jcode-keyless-connectors" && v["version"] == VERSION)
}

fn write_if_changed(path: &Path, text: &str) -> Result<(), String> {
    if std::fs::read_to_string(path).ok().as_deref() != Some(text) {
        std::fs::write(path, text).map_err(|_| "Could not install the local account connector.".to_owned())?;
    }
    Ok(())
}

/// Run on a worker thread. Returns a local page capability, never account tokens.
pub fn open_url() -> Result<String, String> {
    if !cfg!(windows) {
        return Err("The account connector manager currently requires Windows.".into());
    }
    static START_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = START_LOCK.lock().map_err(|_| "Connector startup was interrupted.")?;
    let home = home()?;
    let directory = home.join("connectors");
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_millis(700))
        .build()
        .map_err(|_| "Could not initialize the local connector connection.")?;
    if !healthy(&client) {
        std::fs::create_dir_all(&directory).map_err(|_| "Could not create the connector directory.")?;
        write_if_changed(&directory.join("server.py"), SERVER)?;
        write_if_changed(&directory.join("index.html"), PAGE)?;
        let mut command = std::process::Command::new("python");
        command.arg(directory.join("server.py")).arg("--home").arg(&home)
            .arg("--port").arg(PORT.to_string())
            .env("JCODE_BIN", crate::platform::companion_executable("jcode"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        crate::platform::no_console_window(&mut command);
        command.spawn().map_err(|_| "Could not start the connector. Python 3.11 or newer must be installed.")?;
        for _ in 0..30 {
            if healthy(&client) { break; }
            std::thread::sleep(Duration::from_millis(150));
        }
        if !healthy(&client) {
            return Err("The connector could not start. Check whether local port 42187 is already in use.".into());
        }
    }
    let capability = std::fs::read_to_string(directory.join("local-capability"))
        .map_err(|_| "Could not open the private connector session.")?;
    let capability = capability.trim();
    if capability.len() < 40 || !capability.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err("The private connector session is invalid. Reinstall the connector.".into());
    }
    Ok(format!("http://127.0.0.1:{PORT}/#{capability}"))
}

/// Restore the local transport on desktop startup only after a connector was used.
/// This does not log in, send prompts or query cloud account quotas.
pub fn start_if_configured() {
    if cfg!(windows) && home().is_ok_and(|h| h.join("connectors/local-capability").is_file()) {
        std::thread::spawn(|| { let _ = open_url(); });
    }
}
